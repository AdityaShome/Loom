//! Phase 1 gateway: transparent streaming passthrough to the upstream API.
//! Bytes are forwarded unchanged in both directions; only hop-by-hop headers
//! and Loom's own session header are removed.

use std::{sync::Arc, time::Instant};

use axum::{
    body::Body,
    extract::{Request, State as AxState},
    http::{header, HeaderMap, HeaderName, StatusCode},
    response::Response,
    Router,
};
use futures_util::TryStreamExt;
use loom_core::session::{resolve_session, SESSION_HEADER};

const MAX_BODY: usize = 256 * 1024 * 1024;

pub struct State {
    client: reqwest::Client,
    anthropic: String,
    openai: String,
}

impl State {
    pub fn new(anthropic: String, openai: String) -> Self {
        // No automatic decompression or redirects: forward exactly what upstream sends.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("http client");
        Self {
            client,
            anthropic: anthropic.trim_end_matches('/').to_string(),
            openai: openai.trim_end_matches('/').to_string(),
        }
    }
}

/// OpenAI-style endpoints go to the OpenAI upstream; everything else to Anthropic.
fn is_openai(path: &str) -> bool {
    ["/v1/chat/", "/v1/completions", "/v1/responses", "/v1/embeddings"]
        .iter()
        .any(|p| path.starts_with(p))
}

pub fn router(state: Arc<State>) -> Router {
    // Every path is forwarded, so /v1/messages, /v1/messages/count_tokens, etc. all work.
    Router::new().fallback(proxy).with_state(state)
}

const HOP_BY_HOP: &[&str] = &[
    "connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
    "te", "trailer", "transfer-encoding", "upgrade", "host", "content-length",
];

fn copy_headers(src: &HeaderMap, dst: &mut HeaderMap, strip_loom: bool) {
    let loom = HeaderName::from_static(SESSION_HEADER);
    for (k, v) in src {
        if HOP_BY_HOP.contains(&k.as_str()) || (strip_loom && *k == loom) {
            continue;
        }
        dst.append(k.clone(), v.clone());
    }
}

async fn proxy(AxState(st): AxState<Arc<State>>, req: Request) -> Response {
    let start = Instant::now();
    let (parts, body) = req.into_parts();

    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("read body: {e}")),
    };

    // Session resolution is best-effort and never affects forwarding.
    let hdr = parts.headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok());
    let session = resolve_session(hdr, &body);

    let path = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let base = if is_openai(parts.uri.path()) { &st.openai } else { &st.anthropic };
    let url = format!("{base}{path}");

    let mut headers = HeaderMap::new();
    copy_headers(&parts.headers, &mut headers, true);

    let sent = Instant::now();
    let overhead = sent.duration_since(start);
    let resp = match st
        .client
        .request(parts.method.clone(), &url)
        .headers(headers)
        .body(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, %url, "upstream request failed");
            return error(StatusCode::BAD_GATEWAY, &format!("loom: upstream error: {e}"));
        }
    };

    tracing::info!(
        method = %parts.method,
        path,
        session = session.as_deref().unwrap_or("-"),
        status = resp.status().as_u16(),
        overhead_us = overhead.as_micros() as u64,
        ttfb_ms = sent.elapsed().as_millis() as u64,
        "proxied"
    );

    let mut out = Response::builder().status(resp.status());
    copy_headers(resp.headers(), out.headers_mut().unwrap(), false);
    let stream = resp.bytes_stream().map_err(std::io::Error::other);
    out.body(Body::from_stream(stream))
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "loom: response build failed"))
}

fn error(status: StatusCode, msg: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(msg.to_string()))
        .unwrap()
}
