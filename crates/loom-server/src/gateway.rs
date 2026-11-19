//! Gateway: streaming passthrough to the upstream API.
//! Bytes are forwarded unchanged in both directions (only hop-by-hop headers and
//! Loom's own session header are removed), except for sessions in Enforce mode,
//! whose requests are rewritten first. Any failure in Loom falls back to the
//! original request.

use std::{sync::Arc, time::Instant};

use axum::{
    body::Body,
    extract::{Request, State as AxState},
    http::{header, HeaderMap, HeaderName, StatusCode},
    response::Response,
    Router,
};
use futures_util::TryStreamExt;
use loom_core::{
    model::Provider,
    normalize::normalize,
    session::{resolve_session, SESSION_HEADER},
};
use loom_store::Store;

use crate::summarizer::{Creds, Summarizer, SummarizerConfig};

const MAX_BODY: usize = 256 * 1024 * 1024;

pub struct State {
    client: reqwest::Client,
    anthropic: String,
    openai: String,
    pub store: Option<Arc<Store>>,
    pub summarizer: Option<Arc<Summarizer>>,
    /// Live events for WebSocket subscribers.
    pub events: tokio::sync::broadcast::Sender<Event>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Event {
    #[serde(skip)]
    pub session: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub turn: u32,
    pub new: usize,
    pub missing: usize,
    pub tokens: u32,
}

impl State {
    pub fn new(anthropic: String, openai: String) -> Self {
        // No automatic decompression or redirects: forward exactly what upstream sends.
        let client =
            reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("http client");
        Self {
            client,
            anthropic: anthropic.trim_end_matches('/').to_string(),
            openai: openai.trim_end_matches('/').to_string(),
            store: None,
            summarizer: None,
            events: tokio::sync::broadcast::channel(256).0,
        }
    }

    pub fn with_store(mut self, store: Arc<Store>) -> Self {
        self.store = Some(store);
        self
    }

    /// Enable summarizing (needs a store for the summary cache).
    pub fn with_summarizer(mut self, cfg: SummarizerConfig) -> Self {
        let store = self.store.clone().expect("with_store must be called before with_summarizer");
        self.summarizer = Some(Arc::new(Summarizer::new(cfg, store)));
        self
    }

    pub fn upstreams(&self) -> (&str, &str) {
        (&self.anthropic, &self.openai)
    }
}

/// OpenAI-style endpoints go to the OpenAI upstream; everything else to Anthropic.
fn is_openai(path: &str) -> bool {
    ["/v1/chat/", "/v1/completions", "/v1/responses", "/v1/embeddings"].iter().any(|p| path.starts_with(p))
}

pub fn router(state: Arc<State>) -> Router {
    // Every path is forwarded, so /v1/messages, /v1/messages/count_tokens, etc. all work.
    Router::new().merge(crate::api::routes()).fallback(proxy).with_state(state)
}

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
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

    // Browser requests for the web UI (anything GET outside /v1).
    if parts.method == axum::http::Method::GET && !parts.uri.path().starts_with("/v1/") {
        return crate::ui::serve(&parts.uri).unwrap_or_else(crate::ui::not_built);
    }

    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("read body: {e}")),
    };

    // Session resolution is best-effort and never affects forwarding.
    let hdr = parts.headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok());
    let session = resolve_session(hdr, &body);

    // Conversations are recorded for every chat request. Sessions in Enforce mode are
    // also rewritten before forwarding; every error path forwards the original body.
    let mut body = body;
    if let (Some(store), Some(sid), Some(provider)) =
        (st.store.clone(), session.clone(), ingest_provider(&parts.method, parts.uri.path()))
    {
        let creds = Creds::from_headers(provider, &parts.headers);
        let enforcing = matches!(store.session_mode(&sid).await, Ok(Some(m)) if m == "Enforce");
        if enforcing {
            match crate::enforce::enforce(&store, st.summarizer.as_ref(), &st.events, provider, &sid, &body, &creds)
                .await
            {
                Ok(done) => {
                    tracing::info!(
                        session = %sid,
                        original_tokens = done.original_tokens,
                        sent_tokens = done.sent_tokens,
                        summarized = done.summarized,
                        dropped = done.dropped,
                        rewritten = done.body.is_some(),
                        "enforced"
                    );
                    if let Some(b) = done.body {
                        body = b;
                    }
                }
                Err(e) => tracing::warn!(session = %sid, error = %e, "enforce failed; forwarding the original request"),
            }
        } else {
            let (body, events, summarizer) = (body.clone(), st.events.clone(), st.summarizer.clone());
            tokio::spawn(async move { record(store, summarizer, events, provider, sid, body, creds).await });
        }
    }

    let path = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let base = if is_openai(parts.uri.path()) { &st.openai } else { &st.anthropic };
    let url = format!("{base}{path}");

    let mut headers = HeaderMap::new();
    copy_headers(&parts.headers, &mut headers, true);

    let sent = Instant::now();
    let overhead = sent.duration_since(start);
    let resp = match st.client.request(parts.method.clone(), &url).headers(headers).body(body).send().await {
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

/// Only chat requests carry a conversation worth recording.
fn ingest_provider(method: &axum::http::Method, path: &str) -> Option<Provider> {
    if method != axum::http::Method::POST {
        return None;
    }
    match path {
        "/v1/messages" => Some(Provider::Anthropic),
        "/v1/chat/completions" => Some(Provider::OpenAi),
        _ => None,
    }
}

async fn record(
    store: Arc<Store>,
    summarizer: Option<Arc<Summarizer>>,
    events: tokio::sync::broadcast::Sender<Event>,
    provider: Provider,
    session: String,
    body: bytes::Bytes,
    creds: Creds,
) {
    let n = match normalize(provider, &body) {
        Ok(n) => n,
        Err(e) => return tracing::warn!(%session, error = %e, "skip ingest: unparseable body"),
    };
    match store.ingest(&session, provider, n.model.as_deref(), &n.msgs).await {
        Ok(r) => {
            tracing::info!(
                %session, turn = r.turn, messages = r.total, new = r.new, missing = r.missing, tokens = r.tokens,
                "ingested"
            );
            // In Suggest mode, get summaries ready before they are needed.
            if let Some(sum) = summarizer {
                if matches!(store.session_mode(&session).await, Ok(Some(m)) if m == "Suggest") {
                    tokio::spawn(crate::enforce::prefetch(store.clone(), sum, session.clone(), creds));
                }
            }
            // Err just means nobody is listening.
            let _ = events.send(Event {
                session,
                kind: "turn_completed",
                turn: r.turn,
                new: r.new,
                missing: r.missing,
                tokens: r.tokens,
            });
        }
        Err(e) => tracing::warn!(%session, error = %e, "ingest failed"),
    }
}
