//! Whatever goes wrong inside Loom, the agent's request must still reach the upstream unchanged.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{body::Bytes, extract::State, routing::post, Router};
use loom_server::{gateway, summarizer::SummarizerConfig};
use serde_json::{json, Value};
use sqlx::{Connection, SqliteConnection};

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

type Seen = Arc<Mutex<Vec<Bytes>>>;

struct Env {
    proxy: String,
    c: reqwest::Client,
    seen: Seen,
    db_path: std::path::PathBuf,
    store: Arc<loom_store::Store>,
}

impl Drop for Env {
    fn drop(&mut self) {
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", self.db_path.display()));
        }
    }
}

async fn env(summarizer: Option<SummarizerConfig>) -> Env {
    let seen: Seen = Default::default();
    async fn h(State(seen): State<Seen>, body: Bytes) -> &'static str {
        seen.lock().unwrap().push(body);
        "{}"
    }
    let upstream = serve(Router::new().fallback(post(h)).with_state(seen.clone())).await;
    // A real file so a second connection can damage it behind the store's back.
    let db_path = std::env::temp_dir().join(format!("loom-fail-open-{}-{}.db", std::process::id(), ulid::Ulid::new()));
    let store = Arc::new(loom_store::Store::open(db_path.to_str().unwrap()).await.unwrap());
    let mut state = gateway::State::new(upstream.clone(), upstream).with_store(store.clone());
    if let Some(cfg) = summarizer {
        state = state.with_summarizer(cfg);
    }
    Env { proxy: serve(gateway::router(Arc::new(state))).await, c: reqwest::Client::new(), seen, db_path, store }
}

impl Env {
    async fn raw_db(&self) -> SqliteConnection {
        SqliteConnection::connect(&format!("sqlite://{}", self.db_path.display())).await.unwrap()
    }

    /// Send a request; returns the status and the body the upstream received.
    async fn send(&self, session: &str, body: &Value) -> (u16, Bytes) {
        let before = self.seen.lock().unwrap().len();
        let turns_before = self.store.turn_count(session).await.unwrap_or(0);
        let r = self
            .c
            .post(format!("{}/v1/messages", self.proxy))
            .header("x-loom-session", session)
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        let got = self.seen.lock().unwrap()[before].clone();
        // Background recording lands shortly after; when the store is broken the count never moves,
        // so give up after a moment.
        for _ in 0..40 {
            if self.store.turn_count(session).await.unwrap_or(0) > turns_before {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        (status, got)
    }

    async fn put(&self, session: &str, body: Value) {
        let r = self.c.put(format!("{}/api/sessions/{session}", self.proxy)).json(&body).send().await.unwrap();
        assert_eq!(r.status(), 204);
    }
}

fn transcript() -> Value {
    let mut m = vec![json!({"role": "user", "content": "fix the parser bug"})];
    m.push(json!({"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"cmd": "cat build.log"}}]}));
    m.push(json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "noise line\n".repeat(3000)}]}));
    for i in 0..6 {
        m.push(
            json!({"role": if i % 2 == 0 {"assistant"} else {"user"}, "content": format!("step {i} on the parser")}),
        );
    }
    json!({"model": "claude-x", "system": "be brief", "messages": m})
}

/// An Enforce session that would normally rewrite the request (tiny budget).
async fn enforcing_session(e: &Env, session: &str) -> Value {
    let body = transcript();
    e.send(session, &body).await;
    e.put(session, json!({"mode": "Enforce", "budget_tokens": 400})).await;
    // Sanity: with a healthy store the request really is rewritten.
    let (status, got) = e.send(session, &body).await;
    assert_eq!(status, 200);
    assert!(got.len() < body.to_string().len() / 5, "precondition: Enforce shrinks the request");
    body
}

#[tokio::test]
async fn store_destroyed_mid_session_does_not_break_requests() {
    let e = env(None).await;
    let body = enforcing_session(&e, "s").await;

    let mut raw = e.raw_db().await;
    for table in ["edges", "nodes", "snapshots", "turns", "sessions"] {
        sqlx::query(&format!("DROP TABLE {table}")).execute(&mut raw).await.unwrap();
    }

    // Enforce session: ingest fails, the original is forwarded.
    let (status, got) = e.send("s", &body).await;
    assert_eq!(status, 200);
    assert_eq!(got.as_ref(), body.to_string().as_bytes());
    // A session that never existed: recording fails in the background, nothing else happens.
    let (status, got) = e.send("brand-new", &body).await;
    assert_eq!(status, 200);
    assert_eq!(got.as_ref(), body.to_string().as_bytes());
    // The control API reports an error instead of crashing the server.
    let r = e.c.get(format!("{}/api/sessions", e.proxy)).send().await.unwrap();
    assert_eq!(r.status(), 500);
}

#[tokio::test]
async fn corrupted_node_kind_fails_open() {
    let e = env(None).await;
    let body = enforcing_session(&e, "s").await;

    let mut raw = e.raw_db().await;
    sqlx::query("UPDATE nodes SET kind = 'Nonsense' WHERE rowid = 2").execute(&mut raw).await.unwrap();

    let (status, got) = e.send("s", &body).await;
    assert_eq!(status, 200);
    assert_eq!(got.as_ref(), body.to_string().as_bytes(), "original forwarded untouched");
}

#[tokio::test]
async fn corrupted_content_hash_fails_open() {
    let e = env(None).await;
    let body = enforcing_session(&e, "s").await;

    let mut raw = e.raw_db().await;
    sqlx::query("UPDATE nodes SET content_hash = x'00ff' WHERE rowid = 3").execute(&mut raw).await.unwrap();

    let (status, got) = e.send("s", &body).await;
    assert_eq!(status, 200);
    assert_eq!(got.as_ref(), body.to_string().as_bytes());
}

#[tokio::test]
async fn store_recovers_after_corruption_is_repaired() {
    let e = env(None).await;
    let body = enforcing_session(&e, "s").await;
    let mut raw = e.raw_db().await;
    sqlx::query("UPDATE nodes SET kind = 'Nonsense' WHERE rowid = 2").execute(&mut raw).await.unwrap();
    let (_, got) = e.send("s", &body).await;
    assert_eq!(got.as_ref(), body.to_string().as_bytes());

    sqlx::query("UPDATE nodes SET kind = 'User' WHERE rowid = 2").execute(&mut raw).await.unwrap();
    let (_, got) = e.send("s", &body).await;
    assert!(got.len() < body.to_string().len() / 5, "Enforce works again once the data is sane");
    assert!(e.store.turn_count("s").await.unwrap() >= 3);
}

fn dead_summarizer(url: &str, timeout_ms: u64) -> SummarizerConfig {
    SummarizerConfig {
        provider: loom_core::model::Provider::OpenAi,
        url: url.into(),
        model: "tiny".into(),
        api_key: None,
        timeout: Duration::from_millis(timeout_ms),
        extra: Default::default(),
    }
}

/// Force-summarize the big tool result so Enforce needs a summary.
async fn want_summary(e: &Env, session: &str, body: &Value) {
    e.send(session, body).await;
    let nodes: Value =
        e.c.get(format!("{}/api/sessions/{session}/nodes", e.proxy)).send().await.unwrap().json().await.unwrap();
    let big = nodes.as_array().unwrap().iter().find(|n| n["tokens"].as_u64().unwrap() > 1000).unwrap()["id"].clone();
    e.c.patch(format!("{}/api/nodes/{}", e.proxy, big.as_str().unwrap()))
        .json(&json!({"user_action": "Summarize"}))
        .send()
        .await
        .unwrap();
    e.put(session, json!({"mode": "Enforce"})).await;
}

#[tokio::test]
async fn unreachable_summarizer_falls_back_to_the_original() {
    let e = env(Some(dead_summarizer("http://127.0.0.1:1", 300))).await;
    let body = transcript();
    want_summary(&e, "s", &body).await;
    let t = std::time::Instant::now();
    let (status, got) = e.send("s", &body).await;
    assert_eq!(status, 200);
    assert_eq!(got.as_ref(), body.to_string().as_bytes());
    assert!(t.elapsed() < Duration::from_secs(2), "a dead summarizer must not stall requests");
}

#[tokio::test]
async fn summarizer_returning_garbage_falls_back() {
    // Not JSON at all, then a reply with no text, then an empty summary: each must fall back.
    for reply in ["<html>nope</html>", r#"{"choices":[]}"#, r#"{"choices":[{"message":{"content":"  "}}]}"#] {
        let reply = reply.to_string();
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let r = reply.clone();
                async move { r }
            }),
        );
        let sum_url = serve(app).await;
        let e = env(Some(dead_summarizer(&sum_url, 500))).await;
        let body = transcript();
        want_summary(&e, "s", &body).await;
        let (status, got) = e.send("s", &body).await;
        assert_eq!(status, 200);
        assert_eq!(got.as_ref(), body.to_string().as_bytes());
    }
}
