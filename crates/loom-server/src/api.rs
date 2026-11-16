//! Control API (read-only in Phase 2).

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State as AxState,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
    Json, Router,
};
use loom_core::{
    model::{Action, Node},
    policy::{self, Matcher, Plan, PolicyRule, Weights},
};
use loom_store::{NodePatch, SessionUpdate};
use serde::{Deserialize, Serialize};

use crate::gateway::State;

pub fn routes() -> Router<Arc<State>> {
    Router::new()
        .route("/api/sessions", get(sessions))
        .route("/api/sessions/{id}/nodes", get(nodes))
        .route("/api/sessions/{id}/graph", get(graph))
        .route("/api/sessions/{id}/turns", get(turns))
        .route("/api/sessions/{id}/diff/{a}/{b}", get(diff))
        .route("/ws/sessions/{id}", get(ws))
        .route("/api/sessions/{id}", put(update_session))
        .route("/api/sessions/{id}/plan", get(plan))
        .route("/api/sessions/{id}/match", post(match_rule))
        .route("/api/nodes/{id}", patch(patch_node))
        .route("/api/nodes/{id}/restore", post(restore_node))
        .route("/api/rules", get(list_rules).post(create_rule))
        .route("/api/rules/{id}", put(update_rule).delete(delete_rule))
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

fn view(mut node: Node) -> NodeView {
    let preview: String = node.content.chars().take(200).collect();
    node.content.clear(); // full content stays in the store; lists get a preview
    NodeView { node, preview }
}

async fn nodes(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.list_nodes(&id).await {
        Ok(ns) => Json(ns.into_iter().map(view).collect::<Vec<_>>()).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Serialize)]
struct Graph {
    nodes: Vec<NodeView>,
    edges: Vec<loom_core::graph::Edge>,
}

async fn graph(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match tokio::try_join!(store.list_nodes(&id), store.list_edges(&id)) {
        Ok((ns, edges)) => Json(Graph { nodes: ns.into_iter().map(view).collect(), edges }).into_response(),
        Err(e) => internal(e),
    }
}

// ---------------------------------------------------------------- policy

fn bad(msg: &str) -> Response {
    (StatusCode::BAD_REQUEST, msg.to_string()).into_response()
}

fn not_found(what: &str) -> Response {
    (StatusCode::NOT_FOUND, format!("unknown {what}")).into_response()
}

/// Tell live viewers that the next plan changed.
fn plan_updated(st: &State, session: &str) {
    let _ = st.events.send(crate::gateway::Event {
        session: session.to_string(),
        kind: "plan_updated",
        turn: 0,
        new: 0,
        missing: 0,
        tokens: 0,
    });
}

#[derive(Serialize)]
struct PlanResponse {
    plan: Plan,
    mode: String,
    /// 0 in settings means "model default"; this is the budget actually used.
    budget_tokens: u32,
    budget_is_default: bool,
    weights: Weights,
    /// Edits made since the last model call; they apply to the next one.
    pending_changes: bool,
}

async fn plan(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    let res: anyhow::Result<Option<PlanResponse>> = async {
        let (Some(inp), Some(settings)) = (store.policy_input(&id).await?, store.session_settings(&id).await?) else {
            return Ok(None);
        };
        Ok(Some(PlanResponse {
            plan: policy::plan(&inp),
            mode: settings.mode,
            budget_tokens: inp.budget,
            budget_is_default: settings.budget_tokens <= 0,
            weights: inp.weights,
            pending_changes: store.pending_changes(&id).await?,
        }))
    }
    .await;
    match res {
        Ok(Some(p)) => Json(p).into_response(),
        Ok(None) => not_found("session"),
        Err(e) => internal(e),
    }
}

async fn update_session(AxState(st): AxState<Arc<State>>, Path(id): Path<String>, Json(u): Json<SessionUpdate>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match u.mode.as_deref() {
        None | Some("Observe") | Some("Suggest") => {}
        Some("Enforce") => return bad("Enforce mode is not available yet (it arrives with the rewriter)"),
        Some(_) => return bad("mode must be Observe or Suggest"),
    }
    if u.budget_tokens.is_some_and(|b| b < 0) {
        return bad("budget_tokens must be >= 0 (0 = model default)");
    }
    if let Some(w) = &u.weights {
        let nums = [w.w_imp, w.w_rec, w.w_ref, w.w_task, w.w_sup, w.tau, w.summarize_threshold];
        if nums.iter().any(|x| !x.is_finite()) || w.tau <= 0.0 {
            return bad("weights must be finite numbers and tau must be > 0");
        }
    }
    match store.update_session(&id, &u).await {
        Ok(true) => {
            plan_updated(&st, &id);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("session"),
        Err(e) => internal(e),
    }
}

async fn patch_node(AxState(st): AxState<Arc<State>>, Path(id): Path<String>, Json(p): Json<NodePatch>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    if p.importance.is_some_and(|i| !i.is_finite()) {
        return bad("importance must be a number between 0 and 1");
    }
    match store.patch_node(&id, &p).await {
        Ok(Some(session)) => {
            plan_updated(&st, &session);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(None) => not_found("node"),
        Err(e) => internal(e),
    }
}

async fn restore_node(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.restore_node(&id).await {
        Ok(Some(session)) => {
            plan_updated(&st, &session);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(None) => not_found("node"),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct RuleQuery {
    session: Option<String>,
}

async fn list_rules(AxState(st): AxState<Arc<State>>, axum::extract::Query(q): axum::extract::Query<RuleQuery>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.list_rules(q.session.as_deref()).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct RuleBody {
    session_id: Option<String>,
    matcher: Matcher,
    action: Action,
    #[serde(default)]
    priority: i32,
}

async fn save_rule(st: &State, id: String, b: RuleBody) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    if b.matcher.is_empty() {
        return bad("a rule needs at least one condition (kind, source_glob, min_tokens, older_than_turns)");
    }
    let rule = PolicyRule { id, session_id: b.session_id, matcher: b.matcher, action: b.action, priority: b.priority };
    match store.upsert_rule(&rule).await {
        Ok(()) => {
            match &rule.session_id {
                Some(s) => plan_updated(st, s),
                None => {
                    if let Ok(all) = store.list_sessions().await {
                        all.iter().for_each(|s| plan_updated(st, &s.id));
                    }
                }
            }
            (StatusCode::OK, Json(rule)).into_response()
        }
        Err(e) => internal(e),
    }
}

async fn create_rule(AxState(st): AxState<Arc<State>>, Json(b): Json<RuleBody>) -> Response {
    save_rule(&st, ulid::Ulid::new().to_string(), b).await
}

async fn update_rule(AxState(st): AxState<Arc<State>>, Path(id): Path<String>, Json(b): Json<RuleBody>) -> Response {
    save_rule(&st, id, b).await
}

async fn delete_rule(AxState(st): AxState<Arc<State>>, Path(id): Path<String>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.delete_rule(&id).await {
        Ok(true) => {
            if let Ok(all) = store.list_sessions().await {
                all.iter().for_each(|s| plan_updated(&st, &s.id));
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("rule"),
        Err(e) => internal(e),
    }
}

#[derive(Serialize)]
struct MatchResult {
    count: usize,
    tokens: u32,
    ids: Vec<String>,
}

/// Live match count for the rule editor: which nodes of the next request would this matcher hit?
async fn match_rule(AxState(st): AxState<Arc<State>>, Path(id): Path<String>, Json(m): Json<Matcher>) -> Response {
    let Some(store) = &st.store else { return unavailable() };
    match store.policy_input(&id).await {
        Ok(Some(inp)) => {
            let hits: Vec<_> = inp
                .nodes
                .iter()
                .filter(|n| m.matches(n, inp.latest_turn.saturating_sub(n.first_seen_turn)))
                .collect();
            Json(MatchResult {
                count: hits.len(),
                tokens: hits.iter().map(|n| n.tokens).sum(),
                ids: hits.iter().map(|n| n.id.clone()).collect(),
            })
            .into_response()
        }
        Ok(None) => not_found("session"),
        Err(e) => internal(e),
    }
}
