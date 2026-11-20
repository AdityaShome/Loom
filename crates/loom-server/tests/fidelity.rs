//! The proxy must be invisible: same bytes, same headers, same timing shape as calling the upstream directly.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::post,
    Router,
};
use futures_util::StreamExt;
use loom_server::gateway;

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

async fn proxy_for(upstream: &str) -> String {
    let state = gateway::State::new(upstream.to_string(), upstream.to_string());
    serve(gateway::router(Arc::new(state))).await
}

/// Headers worth comparing: everything except hop-by-hop and per-response values.
fn comparable(h: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    let mut v: Vec<_> = h
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "date" | "content-length" | "transfer-encoding" | "connection"))
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_string()))
        .collect();
    v.sort();
    v
}

/// Gzip-looking bytes plus invalid UTF-8: anything that decodes, re-encodes or assumes text would break this.
fn awkward_bytes() -> Vec<u8> {
    let mut b = vec![0x1f, 0x8b, 0x08, 0x00, 0xff, 0xfe, 0x00, 0x80, 0xc3, 0x28, 0xa0, 0xa1];
    b.extend((0..=255u8).cycle().take(5000));
    b
}

#[tokio::test]
async fn proxy_and_direct_responses_are_identical() {
    let seen: Arc<Mutex<Vec<Bytes>>> = Default::default();
    async fn upstream_handler(State(seen): State<Arc<Mutex<Vec<Bytes>>>>, body: Bytes) -> Response {
        seen.lock().unwrap().push(body);
        Response::builder()
            .status(StatusCode::CREATED)
            .header("content-type", "application/octet-stream")
            .header("content-encoding", "gzip")
            .header("request-id", "req_abc")
            .header("x-ratelimit-remaining", "41")
            .header("set-cookie", "a=1")
            .header("set-cookie", "b=2")
            .body(Body::from(awkward_bytes()))
            .unwrap()
    }
    let upstream = serve(Router::new().fallback(post(upstream_handler)).with_state(seen.clone())).await;
    let proxy = proxy_for(&upstream).await;
    let client = reqwest::Client::new();

    // Same request, once direct and once through Loom (a non-chat path, so no recording is involved).
    let send = |base: String| {
        let c = client.clone();
        async move {
            c.post(format!("{base}/v1/messages/batches?x=1&y=two"))
                .header("x-api-key", "sk-test")
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/octet-stream")
                .body(awkward_bytes())
                .send()
                .await
                .unwrap()
        }
    };
    let direct = send(upstream.clone()).await;
    let via = send(proxy.clone()).await;

    assert_eq!(direct.status(), via.status());
    assert_eq!(comparable(direct.headers()), comparable(via.headers()));
    assert_eq!(via.headers().get_all("set-cookie").iter().count(), 2, "repeated headers survive");
    let (db, vb) = (direct.bytes().await.unwrap(), via.bytes().await.unwrap());
    assert_eq!(db, vb, "response bytes (compressed, not valid UTF-8) are identical");
    assert_eq!(db.as_ref(), awkward_bytes().as_slice());

    // The upstream also received identical request bytes both times.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], seen[1]);
    assert_eq!(seen[1].as_ref(), awkward_bytes().as_slice());
}

#[tokio::test]
async fn streaming_responses_are_not_buffered() {
    // The upstream emits one SSE event, then waits before the next, like a model generating tokens.
    async fn slow_sse(_: HeaderMap) -> Response {
        let stream = futures_util::stream::unfold(0u8, |i| async move {
            if i >= 3 {
                return None;
            }
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            Some((Ok::<_, std::io::Error>(Bytes::from(format!("data: chunk {i}\n\n"))), i + 1))
        });
        Response::builder().header("content-type", "text/event-stream").body(Body::from_stream(stream)).unwrap()
    }
    let upstream = serve(Router::new().fallback(post(slow_sse))).await;
    let proxy = proxy_for(&upstream).await;

    let start = Instant::now();
    let resp = reqwest::Client::new().post(format!("{proxy}/v1/messages")).body("{}").send().await.unwrap();
    let mut stream = resp.bytes_stream();
    let first = stream.next().await.unwrap().unwrap();
    let first_at = start.elapsed();
    assert_eq!(first.as_ref(), b"data: chunk 0\n\n");
    assert!(first_at < Duration::from_millis(200), "first event arrived after {first_at:?}: the proxy is buffering");

    let mut rest = Vec::new();
    while let Some(c) = stream.next().await {
        rest.extend_from_slice(&c.unwrap());
    }
    assert_eq!(rest, b"data: chunk 1\n\ndata: chunk 2\n\n");
    assert!(start.elapsed() >= Duration::from_millis(550), "events kept the upstream's pacing");
}

#[tokio::test]
async fn client_disconnect_mid_stream_does_not_hurt_the_proxy() {
    async fn endless(_: HeaderMap) -> Response {
        let stream = futures_util::stream::unfold(0u32, |i| async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Some((Ok::<_, std::io::Error>(Bytes::from(format!("data: {i}\n\n"))), i + 1))
        });
        Response::builder().header("content-type", "text/event-stream").body(Body::from_stream(stream)).unwrap()
    }
    let upstream = serve(Router::new().route("/v1/messages", post(endless))).await;
    let proxy = proxy_for(&upstream).await;

    let resp = reqwest::Client::new().post(format!("{proxy}/v1/messages")).body("{}").send().await.unwrap();
    let mut stream = resp.bytes_stream();
    for _ in 0..3 {
        stream.next().await.unwrap().unwrap();
    }
    drop(stream); // the agent gives up

    // The proxy keeps serving other requests.
    let r = reqwest::Client::new().get(format!("{proxy}/api/sessions")).send().await.unwrap();
    assert_eq!(r.status(), 503, "no store configured, but the server is alive and answering");
}
