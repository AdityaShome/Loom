use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{body::Bytes, extract::State, routing::post, Json, Router};
use loom_server::{gateway, summarizer::SummarizerConfig};
use serde_json::{json, Value};

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

type Seen = Arc<Mutex<Vec<Bytes>>>;

/// Upstream that records every request body it receives.
async fn recording_upstream() -> (String, Seen) {
    let seen: Seen = Default::default();
    async fn h(State(seen): State<Seen>, body: Bytes) -> &'static str {
        seen.lock().unwrap().push(body);
        "{}"
    }
    let app = Router::new().fallback(post(h)).with_state(seen.clone());
    (serve(app).await, seen)
}

struct Env {
    proxy: String,
    c: reqwest::Client,
    store: Arc<loom_store::Store>,
    seen: Seen,
}

async fn env(summarizer: Option<SummarizerConfig>) -> Env {
    let (upstream, seen) = recording_upstream().await;
    let store = Arc::new(loom_store::Store::open(":memory:").await.unwrap());
    let mut state = gateway::State::new(upstream.clone(), upstream).with_store(store.clone());
    if let Some(cfg) = summarizer {
        state = state.with_summarizer(cfg);
    }
    Env { proxy: serve(gateway::router(Arc::new(state))).await, c: reqwest::Client::new(), store, seen }
}

impl Env {
    /// Send a request and wait until it is fully handled; returns the body the upstream received.
    async fn send(&self, session: &str, body: Value) -> Bytes {
        let before = self.seen.lock().unwrap().len();
        let turns_before = self.store.turn_count(session).await.unwrap();
        let r = self
            .c
            .post(format!("{}/v1/messages", self.proxy))
            .header("x-loom-session", session)
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let upstream_body = self.seen.lock().unwrap()[before].clone();
        // Background recording (Observe/Suggest) finishes shortly after.
        for _ in 0..200 {
            if self.store.turn_count(session).await.unwrap() > turns_before {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        upstream_body
    }
    async fn put(&self, session: &str, body: Value) {
        let r = self.c.put(format!("{}/api/sessions/{session}", self.proxy)).json(&body).send().await.unwrap();
        assert_eq!(r.status(), 204);
    }
    async fn get(&self, path: &str) -> Value {
        self.c.get(format!("{}{path}", self.proxy)).send().await.unwrap().json().await.unwrap()
    }
    async fn wait_turns(&self, session: &str, n: i64) {
        for _ in 0..200 {
            if self.store.turn_count(session).await.unwrap() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("turns did not reach {n}");
    }
}

/// task, then a huge old tool exchange, then a few ordinary messages.
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

fn node_with_big_result(nodes: &Value) -> String {
    nodes.as_array().unwrap().iter().find(|n| n["tokens"].as_u64().unwrap() > 1000).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn enforce_drops_old_bulk_and_forwards_a_valid_smaller_request() {
    let e = env(None).await;
    let original = transcript();
    e.send("e", original.clone()).await; // Observe: recorded, forwarded untouched
    e.put("e", json!({"mode": "Enforce", "budget_tokens": 400})).await;

    let sent = e.send("e", original.clone()).await;
    let got: Value = serde_json::from_slice(&sent).unwrap();
    assert!(sent.len() < original.to_string().len() / 5, "request shrank: {} bytes", sent.len());
    assert!(!sent.windows(10).any(|w| w == b"noise line"), "the big tool result is gone");
    assert!(!got.to_string().contains("\"t1\""), "its tool call went with it (pair stays together)");
    assert_eq!(got["system"], "be brief");
    assert_eq!(got["messages"][0]["content"], "fix the parser bug");
    loom_core::rewrite::validate_request(loom_core::model::Provider::Anthropic, &got).unwrap();

    // The turn records what the model actually received.
    let turns = e.get("/api/sessions/e/turns").await;
    let t = &turns.as_array().unwrap()[1];
    assert_eq!(t["rewritten"], true);
    assert!(t["sent_tokens"].as_i64().unwrap() < t["request_tokens"].as_i64().unwrap() / 5);
    let s = &e.get("/api/sessions").await.as_array().unwrap()[0].clone();
    assert!(s["saved_tokens"].as_i64().unwrap() > 1000);

    // Node status reflects the decision, and the agent can bring the node back.
    let nodes = e.get("/api/sessions/e/nodes").await;
    let big = node_with_big_result(&nodes);
    let status = nodes.as_array().unwrap().iter().find(|n| n["id"] == big.as_str()).unwrap()["status"].clone();
    assert_eq!(status, "Dropped");
    e.c.post(format!("{}/api/nodes/{big}/restore", e.proxy)).send().await.unwrap();
    e.put("e", json!({"budget_tokens": 100_000})).await;
    let sent = e.send("e", original.clone()).await;
    assert!(sent.windows(10).any(|w| w == b"noise line"), "restored node is sent again");
}

#[tokio::test]
async fn enforce_is_byte_identical_when_nothing_changes() {
    let e = env(None).await;
    // Create the session row first (PUT on an unknown session is a 404); the default budget is
    // huge, so Enforce has nothing to drop.
    e.send("e", transcript()).await;
    e.put("e", json!({"mode": "Enforce"})).await;
    let original = transcript().to_string();
    let sent = e.send("e", transcript()).await;
    assert_eq!(sent.as_ref(), original.as_bytes(), "untouched request keeps its exact bytes");
}

#[tokio::test]
async fn enforce_fails_open_on_bad_bodies() {
    let e = env(None).await;
    e.send("e", transcript()).await;
    e.put("e", json!({"mode": "Enforce"})).await;
    let r =
        e.c.post(format!("{}/v1/messages", e.proxy))
            .header("x-loom-session", "e")
            .body("this is not json")
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(e.seen.lock().unwrap().last().unwrap().as_ref(), b"this is not json");
}

/// Fake summarizer (OpenAI chat format) with a configurable delay and failure mode.
async fn fake_summarizer(delay_ms: u64, fail: bool) -> (String, Arc<Mutex<u32>>) {
    let calls = Arc::new(Mutex::new(0u32));
    let c2 = calls.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(_): Json<Value>| {
            let c = c2.clone();
            async move {
                *c.lock().unwrap() += 1;
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                if fail {
                    return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "boom"})));
                }
                (axum::http::StatusCode::OK, Json(json!({"choices": [{"message": {"content": "<think>hmm</think>build.log is full of noise lines"}}]})))
            }
        }),
    );
    (serve(app).await, calls)
}

fn cfg(url: String, timeout_ms: u64) -> SummarizerConfig {
    SummarizerConfig {
        provider: loom_core::model::Provider::OpenAi,
        url,
        model: "tiny".into(),
        api_key: None,
        timeout: Duration::from_millis(timeout_ms),
        extra: Default::default(),
    }
}

#[tokio::test]
async fn slow_summaries_fall_back_then_are_used_on_the_next_call() {
    let (sum_url, calls) = fake_summarizer(400, false).await;
    let e = env(Some(cfg(sum_url, 100))).await;
    let original = transcript();
    e.send("s", original.clone()).await;
    let nodes = e.get("/api/sessions/s/nodes").await;
    let big = node_with_big_result(&nodes);
    e.c.patch(format!("{}/api/nodes/{big}", e.proxy)).json(&json!({"user_action": "Summarize"})).send().await.unwrap();
    e.put("s", json!({"mode": "Enforce"})).await;

    // The summary is not ready within 100 ms: fall back to the original content (it fits the budget).
    let t = std::time::Instant::now();
    let sent = e.send("s", original.clone()).await;
    assert!(t.elapsed() < Duration::from_millis(380), "request must not wait for the slow summarizer");
    assert!(sent.windows(10).any(|w| w == b"noise line"), "fallback keeps the original");

    // The detached job finishes and caches the summary.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(*calls.lock().unwrap(), 1);

    let sent = e.send("s", original.clone()).await;
    let got: Value = serde_json::from_slice(&sent).unwrap();
    let s = got.to_string();
    assert!(s.contains("[loom summary of 1 item] build.log is full of noise lines"), "{s}");
    assert!(!s.contains("<think>") && !s.contains("noise line\\n"));
    assert!(s.contains("\"t1\""), "tool call stays verbatim next to its summarized result");
    loom_core::rewrite::validate_request(loom_core::model::Provider::Anthropic, &got).unwrap();
    assert_eq!(*calls.lock().unwrap(), 1, "cached: no second summarizer call");
    let nodes = e.get("/api/sessions/s/nodes").await;
    assert_eq!(nodes.as_array().unwrap().iter().find(|n| n["id"] == big.as_str()).unwrap()["status"], "Summarized");
}

#[tokio::test]
async fn failing_summarizer_never_breaks_requests() {
    let (sum_url, calls) = fake_summarizer(0, true).await;
    let e = env(Some(cfg(sum_url, 200))).await;
    let original = transcript();
    e.send("f", original.clone()).await;
    let big = node_with_big_result(&e.get("/api/sessions/f/nodes").await);
    e.c.patch(format!("{}/api/nodes/{big}", e.proxy)).json(&json!({"user_action": "Summarize"})).send().await.unwrap();
    e.put("f", json!({"mode": "Enforce"})).await;
    let sent = e.send("f", original.clone()).await;
    assert_eq!(sent.as_ref(), original.to_string().as_bytes(), "falls back to the untouched original");
    assert!(*calls.lock().unwrap() >= 1);
}

#[tokio::test]
async fn suggest_mode_prefetches_summaries() {
    let (sum_url, calls) = fake_summarizer(0, false).await;
    let e = env(Some(cfg(sum_url, 200))).await;
    let original = transcript();
    e.send("p", original.clone()).await;
    let big = node_with_big_result(&e.get("/api/sessions/p/nodes").await);
    e.c.patch(format!("{}/api/nodes/{big}", e.proxy)).json(&json!({"user_action": "Summarize"})).send().await.unwrap();
    e.put("p", json!({"mode": "Suggest"})).await;
    e.send("p", original.clone()).await;
    e.wait_turns("p", 2).await;
    for _ in 0..100 {
        if *calls.lock().unwrap() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(*calls.lock().unwrap(), 1, "summary was generated ahead of time");
    // Suggest still forwards the original untouched.
    assert_eq!(e.seen.lock().unwrap().last().unwrap().as_ref(), original.to_string().as_bytes());
}
