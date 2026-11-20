//! Proxy overhead benchmark.
//!
//!     cargo run --release -p loom-server --example bench
//!     cargo run --release -p loom-server --example bench -- --requests 500 --sizes 10,100,1000,3000
//!
//! Measures end-to-end request latency against an in-process upstream that answers instantly, so the
//! difference between a row and the "direct" row is what Loom adds. Every request carries `size` messages
//! of which the last two are new, like a normal agent turn. Run in release mode: debug builds are far slower.

use std::{sync::Arc, time::Instant};

use axum::{routing::post, Router};
use loom_server::gateway;
use loom_store::{SessionUpdate, Store};
use serde_json::{json, Value};

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

/// A realistic conversation of `size` messages; `turn` makes the last two unique.
fn conversation(size: usize, turn: usize) -> Value {
    let filler = "the parser handles nested expressions and reports errors with line numbers. ".repeat(4);
    let mut m = vec![json!({"role": "user", "content": "refactor the parser module"})];
    let mut i = 1;
    while m.len() + 2 < size {
        if i % 5 == 0 {
            let id = format!("t{i}");
            m.push(json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "Read", "input": {"file_path": format!("/repo/src/f{}.rs", i % 7)}}]}));
            m.push(json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": format!("{filler}{}", "fn body() {}\n".repeat(150))}]}));
        } else {
            m.push(json!({"role": "assistant", "content": format!("step {i}: {filler}")}));
            m.push(json!({"role": "user", "content": format!("ok, continue with {i}")}));
        }
        i += 1;
    }
    m.push(json!({"role": "assistant", "content": format!("fresh assistant text for turn {turn}")}));
    m.push(json!({"role": "user", "content": format!("fresh user text for turn {turn}")}));
    json!({"model": "claude-bench", "system": "You are a coding agent.", "messages": m})
}

struct Stats {
    p50: f64,
    p95: f64,
    p99: f64,
}

fn stats(mut us: Vec<f64>) -> Stats {
    us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |q: f64| us[((us.len() as f64 * q) as usize).min(us.len() - 1)];
    Stats { p50: at(0.50), p95: at(0.95), p99: at(0.99) }
}

async fn measure(
    client: &reqwest::Client,
    url: &str,
    session: &str,
    size: usize,
    requests: usize,
    base: usize,
) -> Stats {
    let mut samples = Vec::with_capacity(requests);
    for turn in 0..requests + 20 {
        let body = conversation(size, base + turn).to_string();
        let t = Instant::now();
        let r = client.post(url).header("x-loom-session", session).body(body).send().await.unwrap();
        let _ = r.bytes().await.unwrap();
        if turn >= 20 {
            samples.push(t.elapsed().as_secs_f64() * 1e6); // skip warm-up
        }
    }
    stats(samples)
}

/// Recording happens in the background; wait until the session exists before changing its mode.
async fn wait_for_session(store: &Store, session: &str) {
    for _ in 0..500 {
        if store.session_settings(session).await.unwrap().is_some() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("session {session} was never recorded");
}

/// Observe-mode recording is asynchronous; let it finish so it does not distort the next case.
async fn wait_for_turns(store: &Store, session: &str, n: i64) {
    for _ in 0..3000 {
        if store.turn_count(session).await.unwrap_or(0) >= n {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("session {session} stopped at {} of {n} turns", store.turn_count(session).await.unwrap_or(0));
}

fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned())
}

#[tokio::main]
async fn main() {
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    }
    let requests: usize = arg("--requests").and_then(|v| v.parse().ok()).unwrap_or(200);
    let sizes: Vec<usize> = arg("--sizes")
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![10, 100, 1000]);

    // Read the whole body like a real API would, or large requests get their connection reset.
    let upstream = serve(
        Router::new()
            .fallback(post(|body: axum::body::Bytes| async move {
                drop(body);
                "{}"
            }))
            .layer(axum::extract::DefaultBodyLimit::disable()),
    )
    .await;
    let db = std::env::temp_dir().join(format!("loom-bench-{}.db", std::process::id()));
    let store = Arc::new(Store::open(db.to_str().unwrap()).await.unwrap());
    let state = gateway::State::new(upstream.clone(), upstream.clone()).with_store(store.clone());
    let proxy = serve(gateway::router(Arc::new(state))).await;
    let client = reqwest::Client::new();

    println!("{requests} requests per cell (after 20 warm-up), sequential, release build assumed\n");
    println!("| messages | case | p50 | p95 | p99 | added p50 |");
    println!("|---:|---|---:|---:|---:|---:|");
    for &size in &sizes {
        let direct = measure(&client, &format!("{upstream}/v1/messages"), "direct", size, requests, 0).await;
        let row = |case: &str, s: &Stats| {
            println!(
                "| {size} | {case} | {:.2} ms | {:.2} ms | {:.2} ms | {} |",
                s.p50 / 1e3,
                s.p95 / 1e3,
                s.p99 / 1e3,
                if case == "direct (no Loom)" {
                    "-".to_string()
                } else {
                    format!("{:+.2} ms", (s.p50 - direct.p50) / 1e3)
                }
            )
        };
        row("direct (no Loom)", &direct);

        let url = format!("{proxy}/v1/messages");
        // Observe: forward immediately, record in the background.
        let s = measure(&client, &url, &format!("observe-{size}"), size, requests, 1_000).await;
        row("Loom, Observe", &s);
        wait_for_turns(&store, &format!("observe-{size}"), (requests + 20) as i64).await;

        // Enforce with a budget that fits everything: ingest + plan, original bytes forwarded.
        let session = format!("enforce-keep-{size}");
        measure(&client, &url, &session, size, 1, 2_000).await; // creates the session
        wait_for_session(&store, &session).await;
        let s_update = SessionUpdate { mode: Some("Enforce".into()), ..Default::default() };
        assert!(store.update_session(&session, &s_update).await.unwrap());
        let s = measure(&client, &url, &session, size, requests, 3_000).await;
        row("Loom, Enforce (nothing to change)", &s);

        // Enforce with a budget that forces roughly half of the context out: ingest + plan + rewrite + validate.
        let session = format!("enforce-rewrite-{size}");
        measure(&client, &url, &session, size, 1, 4_000).await;
        wait_for_session(&store, &session).await;
        let tokens = loom_core::matching::count_tokens(&conversation(size, 0).to_string()) as i64;
        let s_update =
            SessionUpdate { mode: Some("Enforce".into()), budget_tokens: Some(tokens / 2), ..Default::default() };
        assert!(store.update_session(&session, &s_update).await.unwrap());
        let s = measure(&client, &url, &session, size, requests, 5_000).await;
        row("Loom, Enforce (rewrites ~half)", &s);
        // Prove the rewrite really happened: how much of the agent's request did the model receive?
        let turns = store.list_turns(&session).await.unwrap();
        let steady: Vec<_> = turns.iter().filter(|t| t.idx >= 21).collect();
        let rewritten = steady.iter().filter(|t| t.rewritten).count();
        let (orig, sent): (i64, i64) = steady.iter().fold((0, 0), |a, t| (a.0 + t.request_tokens, a.1 + t.sent_tokens));
        println!(
            "|  | _check: {rewritten}/{} requests rewritten; the model received {:.0}% of the tokens the agent sent_ | | | | |",
            steady.len(),
            100.0 * sent as f64 / orig.max(1) as f64
        );
    }
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", db.display()));
    }
}
