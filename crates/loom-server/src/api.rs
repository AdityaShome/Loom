//! Control API (read-only in Phase 2).

use std::sync::Arc;

use axum::{
    extract::{Path, State as AxState},
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
