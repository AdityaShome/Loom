use std::{sync::Arc, time::Duration};

use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::post,
    Router,
};
use loom_server::gateway;

const SSE: &[&str] = &[
    "event: message_start\ndata: {\"type\":\"message_start\"}\n\n",
    "event: content_block_delta\ndata: {\"delta\":{\"text\":\"h\u{e9}llo\"}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
];

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

/// Fake Anthropic: streams SSE in separate chunks and reports what it received.
async fn fake_upstream(headers: HeaderMap, req: Request) -> Response {
    let body = axum::body::to_bytes(req.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"{\"messages\":[]}");
    assert_eq!(headers.get("x-api-key").unwrap(), "sk-test");
    assert!(headers.get("x-loom-session").is_none(), "loom header must not leak upstream");
    let stream = futures_util::stream::iter(SSE.iter().map(|c| Ok::<_, std::io::Error>(Bytes::from(*c))))
        .then(|c| async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            c
        });
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("request-id", "req_123")
        .body(Body::from_stream(stream))
        .unwrap()
}

use futures_util::StreamExt;

#[tokio::test]
async fn streams_byte_identical_and_passes_headers() {
    let upstream = serve(Router::new().route("/v1/messages", post(fake_upstream))).await;
    let proxy = serve(gateway::router(Arc::new(gateway::State::new(upstream.clone(), upstream)))).await;

    let resp = reqwest::Client::new()
        .post(format!("{proxy}/v1/messages"))
        .header("x-api-key", "sk-test")
        .header("x-loom-session", "s1")
        .body("{\"messages\":[]}")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    assert_eq!(resp.headers()["request-id"], "req_123");
    let got = resp.bytes().await.unwrap();
    assert_eq!(got.as_ref(), SSE.concat().as_bytes());
}

#[tokio::test]
async fn upstream_down_returns_502() {
    let proxy = serve(gateway::router(Arc::new(gateway::State::new("http://127.0.0.1:1".into(), "http://127.0.0.1:1".into())))).await;
    let resp = reqwest::Client::new().post(format!("{proxy}/v1/messages")).body("{}").send().await.unwrap();
    assert_eq!(resp.status(), 502);
}

#[tokio::test]
async fn routes_openai_paths_to_openai_upstream() {
    let anthropic = serve(Router::new().fallback(|| async { "anthropic" })).await;
    let openai = serve(Router::new().fallback(|| async { "openai" })).await;
    let proxy = serve(gateway::router(Arc::new(gateway::State::new(anthropic, openai)))).await;
    let c = reqwest::Client::new();
    let a = c.post(format!("{proxy}/v1/messages")).body("{}").send().await.unwrap().text().await.unwrap();
    let o = c.post(format!("{proxy}/v1/chat/completions")).body("{}").send().await.unwrap().text().await.unwrap();
    assert_eq!((a.as_str(), o.as_str()), ("anthropic", "openai"));
}

#[tokio::test]
async fn records_conversation_and_serves_nodes() {
    let upstream = serve(Router::new().fallback(|| async { "{}" })).await;
    let store = Arc::new(loom_store::Store::open(":memory:").await.unwrap());
    let state = gateway::State::new(upstream.clone(), upstream).with_store(store);
    let proxy = serve(gateway::router(Arc::new(state))).await;
    let c = reqwest::Client::new();

    let req = |msgs: serde_json::Value| {
        c.post(format!("{proxy}/v1/messages"))
            .header("x-loom-session", "e2e")
            .body(serde_json::json!({"model": "m", "system": "S", "messages": msgs}).to_string())
    };
    let u = serde_json::json!({"role": "user", "content": "hi"});
    let a = serde_json::json!({"role": "assistant", "content": "hello"});
    let u2 = serde_json::json!({"role": "user", "content": "more"});
    assert_eq!(req(serde_json::json!([u])).send().await.unwrap().status(), 200);
    assert_eq!(req(serde_json::json!([u, a, u2])).send().await.unwrap().status(), 200);

    // Ingest runs in the background; poll until both turns land.
    let mut nodes: Vec<serde_json::Value> = Vec::new();
    for _ in 0..50 {
        nodes = c.get(format!("{proxy}/api/sessions/e2e/nodes")).send().await.unwrap().json().await.unwrap();
        if nodes.len() == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(nodes.len(), 4); // system, hi, hello, more
    assert_eq!(nodes[1]["preview"], "hi");
    assert_eq!(nodes[1]["kind"], "User");

    let sessions: Vec<serde_json::Value> =
        c.get(format!("{proxy}/api/sessions")).send().await.unwrap().json().await.unwrap();
    assert_eq!(sessions[0]["id"], "e2e");
    assert_eq!(sessions[0]["turns"], 2);
}
