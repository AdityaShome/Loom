//! HTTP API for the knowledge graph and for editing node text.

use std::sync::Arc;

use axum::{
    extract::{Path, State as AxState},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use loom_core::{
    knowledge::render_memory,
    matching::{count_tokens, message_overhead},
    model::{EntityKind, Provider},
};
use loom_store::{EntityPatch, FactPatch, KgError};
use serde::{Deserialize, Serialize};

use crate::gateway::{Event, State};

pub fn routes() -> Router<Arc<State>> {
    Router::new()
        .route("/api/sessions/{id}/knowledge", get(knowledge))
        .route("/api/sessions/{id}/knowledge/entities", post(create_entity))
        .route("/api/knowledge/entities/{id}", patch(update_entity).delete(delete_entity))
        .route("/api/sessions/{id}/knowledge/facts", post(create_fact))
        .route("/api/knowledge/facts/{id}", patch(update_fact).delete(delete_fact))
        .route("/api/nodes/{id}", get(node_detail))
        .route("/api/nodes/{id}/content", put(set_content).merge(delete(clear_content)))
}

fn unavailable() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "loom: store unavailable").into_response()
}

fn error(e: KgError) -> Response {
    match e {
        KgError::NotFound(what) => (StatusCode::NOT_FOUND, format!("unknown {what}")).into_response(),
        KgError::Conflict(m) => (StatusCode::CONFLICT, m).into_response(),
        KgError::Invalid(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        KgError::Other(e) => {
            tracing::error!(error = %e, "knowledge api error");
            (StatusCode::INTERNAL_SERVER_ERROR, "loom: internal error").into_response()
        }
    }
}

/// Tell live viewers the graph changed. Notes and edits also change the plan (they cost budget).
fn changed(st: &State, session: &str, plan_too: bool) {
    let ev = |kind| Event { session: session.to_string(), kind, turn: 0, new: 0, missing: 0, tokens: 0 };
    let _ = st.events.send(ev("knowledge_updated"));
    if plan_too {
        let _ = st.events.send(ev("plan_updated"));
    }
}

#[derive(Serialize)]
struct Memory {
    /// Exactly what is appended to the model's request in Enforce mode.
    text: String,
    tokens: u32,
    facts: usize,
}

#[derive(Serialize)]
struct KnowledgeResponse {
    #[serde(flatten)]
    graph: loom_store::Knowledge,
    memory: Option<Memory>,
    /// Nodes whose text you replaced.
    edited_nodes: Vec<String>,
}

async fn knowledge(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    let res: anyhow::Result<Option<KnowledgeResponse>> = async {
        let Some(settings) = store.session_settings(&id).await? else { return Ok(None) };
        let provider = settings.provider.as_deref().and_then(Provider::parse).unwrap_or(Provider::Anthropic);
        let graph = store.knowledge(&id).await?;
        let injected = store.injected_facts(&id).await?;
        let memory = render_memory(&injected).map(|text| Memory {
            tokens: count_tokens(&text) + message_overhead(provider),
            text,
            facts: injected.len(),
        });
        let edited_nodes = store.list_nodes(&id).await?.into_iter().filter(|n| n.edited).map(|n| n.id).collect();
        Ok(Some(KnowledgeResponse { graph, memory, edited_nodes }))
    }
    .await;
    match res {
        Ok(Some(k)) => Json(k).into_response(),
        Ok(None) => error(KgError::NotFound("session")),
        Err(e) => error(e.into()),
    }
}

#[derive(Deserialize)]
struct NewEntity {
    kind: EntityKind,
    name: String,
    note: Option<String>,
}

async fn create_entity(
    AxState(st): AxState<Arc<State>>,
    Path(session): Path<String>,
    Json(b): Json<NewEntity>,
) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.create_entity(&session, b.kind, &b.name, b.note.as_deref()).await {
        Ok(e) => {
            changed(&st, &session, false);
            (StatusCode::CREATED, Json(e)).into_response()
        }
        Err(e) => error(e),
    }
}

async fn update_entity(
    AxState(st): AxState<Arc<State>>,
    Path(id): Path<String>,
    Json(p): Json<EntityPatch>,
) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.update_entity(&id, &p).await {
        Ok(e) => {
            changed(&st, &e.session_id, true);
            Json(e).into_response()
        }
        Err(e) => error(e),
    }
}

async fn delete_entity(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.delete_entity(&id).await {
        Ok(session) => {
            changed(&st, &session, true);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => error(e),
    }
}

#[derive(Deserialize)]
struct NewFact {
    subject_id: String,
    predicate: String,
    object_id: Option<String>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    inject: bool,
}

async fn create_fact(
    AxState(st): AxState<Arc<State>>,
    Path(session): Path<String>,
    Json(b): Json<NewFact>,
) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.create_fact(&session, &b.subject_id, &b.predicate, b.object_id.as_deref(), &b.text, b.inject).await {
        Ok(f) => {
            changed(&st, &session, f.inject);
            (StatusCode::CREATED, Json(f)).into_response()
        }
        Err(e) => error(e),
    }
}

async fn update_fact(AxState(st): AxState<Arc<State>>, Path(id): Path<String>, Json(p): Json<FactPatch>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.update_fact(&id, &p).await {
        Ok(f) => {
            changed(&st, &f.session_id, true);
            Json(f).into_response()
        }
        Err(e) => error(e),
    }
}

async fn delete_fact(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.delete_fact(&id).await {
        Ok(session) => {
            changed(&st, &session, true);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => error(e),
    }
}

async fn node_detail(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.node_detail(&id).await {
        Ok(Some(d)) => Json(d).into_response(),
        Ok(None) => error(KgError::NotFound("node")),
        Err(e) => error(e.into()),
    }
}

#[derive(Deserialize)]
struct NewContent {
    text: String,
}

async fn set_content(AxState(st): AxState<Arc<State>>, Path(id): Path<String>, Json(b): Json<NewContent>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.set_override(&id, &b.text).await {
        Ok(session) => {
            changed(&st, &session, true);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => error(e),
    }
}

async fn clear_content(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.clear_override(&id).await {
        Ok(session) => {
            changed(&st, &session, true);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => error(e),
    }
}
