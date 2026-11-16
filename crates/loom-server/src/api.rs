//! Control API (read-only in Phase 2).

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State as AxState,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use loom_core::model::Node;
use serde::Serialize;

use crate::gateway::State;

pub fn routes() -> Router<Arc<State>> {
    Router::new()
        .route("/api/sessions", get(sessions))
        .route("/api/sessions/{id}/nodes", get(nodes))
        .route("/api/sessions/{id}/turns", get(turns))
        .route("/api/sessions/{id}/diff/{a}/{b}", get(diff))
        .route("/ws/sessions/{id}", get(ws))
}

async fn turns(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.list_turns(&id).await {
        Ok(t) => Json(t).into_response(),
        Err(e) => internal(e),
    }
}

async fn diff(AxState(st): AxState<Arc<State>>, Path((id, a, b)): Path<(String, u32, u32)>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.diff_turns(&id, a, b).await {
        Ok(Some(d)) => Json(d).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "unknown turn").into_response(),
        Err(e) => internal(e),
    }
}

/// Live events for one session: `{"type":"turn_completed","turn":N,...}`.
async fn ws(AxState(st): AxState<Arc<State>>, Path(id): Path<String>, up: WebSocketUpgrade) -> Response {
    let rx = st.events.subscribe();
    up.on_upgrade(move |socket| pump(socket, rx, id))
}

async fn pump(mut socket: WebSocket, mut rx: tokio::sync::broadcast::Receiver<crate::gateway::Event>, id: String) {
    use tokio::sync::broadcast::error::RecvError;
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) if ev.session == id => {
                    let Ok(json) = serde_json::to_string(&ev) else { continue };
                    if socket.send(Message::Text(json.into())).await.is_err() { break }
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            // Client went away (or sent something we ignore).
            msg = socket.recv() => match msg {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            },
        }
    }
}

#[derive(Serialize)]
struct NodeView {
    #[serde(flatten)]
    node: Node,
    preview: String,
}

fn unavailable() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "loom: store unavailable").into_response()
}

fn internal(e: anyhow::Error) -> Response {
    tracing::error!(error = %e, "api error");
    (StatusCode::INTERNAL_SERVER_ERROR, "loom: internal error").into_response()
}

async fn sessions(AxState(st): AxState<Arc<State>>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.list_sessions().await {
        Ok(s) => Json(s).into_response(),
        Err(e) => internal(e),
    }
}

async fn nodes(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.list_nodes(&id).await {
        Ok(ns) => Json(
            ns.into_iter()
                .map(|mut node| {
                    let preview: String = node.content.chars().take(200).collect();
                    node.content.clear(); // full content stays in the store; the list view gets a preview
                    NodeView { node, preview }
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => internal(e),
    }
}
