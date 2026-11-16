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
    let proxy = serve(gateway::router(Arc::new(gateway::State::new(upstream)))).await;

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
    let proxy = serve(gateway::router(Arc::new(gateway::State::new("http://127.0.0.1:1".into())))).await;
    let resp = reqwest::Client::new().post(format!("{proxy}/v1/messages")).body("{}").send().await.unwrap();
    assert_eq!(resp.status(), 502);
}
