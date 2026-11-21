//! The user's edits (replaced text, notes) reach the model in Enforce mode and only there.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{body::Bytes, extract::State, routing::post, Router};
use loom_core::model::{EntityKind, NodeKind};
use loom_server::gateway;
use serde_json::{json, Value};

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

type Seen = Arc<Mutex<Vec<Bytes>>>;

struct Env {
    proxy: String,
    c: reqwest::Client,
    store: Arc<loom_store::Store>,
    seen: Seen,
}

async fn env() -> Env {
    let seen: Seen = Default::default();
    async fn h(State(seen): State<Seen>, body: Bytes) -> &'static str {
        seen.lock().unwrap().push(body);
        "{}"
    }
    let upstream = serve(Router::new().fallback(post(h)).with_state(seen.clone())).await;
    let store = Arc::new(loom_store::Store::open(":memory:").await.unwrap());
    let state = gateway::State::new(upstream.clone(), upstream).with_store(store.clone());
    Env { proxy: serve(gateway::router(Arc::new(state))).await, c: reqwest::Client::new(), store, seen }
}

impl Env {
    /// Send to the Anthropic or OpenAI route; returns the body the upstream received.
    async fn send(&self, path: &str, session: &str, body: &Value) -> Value {
        let before = self.seen.lock().unwrap().len();
        let turns_before = self.store.turn_count(session).await.unwrap();
        let r = self
            .c
            .post(format!("{}{path}", self.proxy))
            .header("x-loom-session", session)
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        for _ in 0..200 {
            if self.store.turn_count(session).await.unwrap() > turns_before {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let got = self.seen.lock().unwrap()[before].clone();
        serde_json::from_slice(&got).unwrap()
    }

    async fn mode(&self, session: &str, mode: &str, budget: Option<i64>) {
        let u = loom_store::SessionUpdate { mode: Some(mode.into()), budget_tokens: budget, ..Default::default() };
        assert!(self.store.update_session(session, &u).await.unwrap());
    }
}

fn anthropic() -> Value {
    let mut m = vec![json!({"role": "user", "content": "fix the parser bug"})];
    m.push(json!({"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/r/parser.rs"}}]}));
    m.push(json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "fn parse() { /* the original body */ }"}]}));
    m.push(json!({"role": "assistant", "content": "I see the problem"}));
    m.push(json!({"role": "user", "content": "go ahead"}));
    json!({"model": "claude-x", "system": "be brief", "messages": m})
}

fn openai() -> Value {
    json!({"model": "gpt-x", "messages": [
        {"role": "system", "content": "be brief"},
        {"role": "user", "content": "fix the parser bug"},
        {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"/r/parser.rs\"}"}}]},
        {"role": "tool", "tool_call_id": "c1", "content": "fn parse() { /* the original body */ }"}]})
}

/// Create the session by sending the request once in Observe mode.
async fn start(e: &Env, path: &str, session: &str, body: &Value) {
    e.send(path, session, body).await;
}

async fn add_note(e: &Env, session: &str, subject: &str, text: &str) {
    let ent = e.store.create_entity(session, EntityKind::Concept, subject, None).await.unwrap();
    e.store.create_fact(session, &ent.id, "note", None, text, true).await.unwrap();
}

async fn node_with(e: &Env, session: &str, kind: NodeKind) -> String {
    e.store
        .list_nodes(session)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.kind == kind || (kind == NodeKind::ToolResult && n.kind == NodeKind::File))
        .unwrap()
        .id
}

#[tokio::test]
async fn notes_you_marked_are_appended_to_the_last_user_message_in_enforce() {
    let e = env().await;
    let body = anthropic();
    start(&e, "/v1/messages", "s", &body).await;
    add_note(&e, "s", "parser", "parse() must never panic").await;
    e.mode("s", "Enforce", None).await;

    let sent = e.send("/v1/messages", "s", &body).await;
    let msgs = sent["messages"].as_array().unwrap();
    let last = msgs.last().unwrap()["content"].as_str().unwrap();
    assert!(last.starts_with("go ahead\n\n[loom notes]"), "{last}");
    assert!(last.contains("- parser: parse() must never panic"));
    // everything before the last message is exactly what the agent sent
    assert_eq!(sent["system"], body["system"]);
    assert_eq!(&msgs[..msgs.len() - 1], &body["messages"].as_array().unwrap()[..msgs.len() - 1]);

    // the turn record says the model received something different, and counts the notes' tokens
    let turns = e.store.list_turns("s").await.unwrap();
    let t = turns.last().unwrap();
    assert!(t.rewritten);
    assert!(t.sent_tokens > t.request_tokens, "notes add tokens: {} vs {}", t.sent_tokens, t.request_tokens);
}

#[tokio::test]
async fn facts_you_did_not_mark_are_never_sent() {
    let e = env().await;
    let body = anthropic();
    start(&e, "/v1/messages", "s", &body).await;
    let ent = e.store.create_entity("s", EntityKind::Concept, "private", None).await.unwrap();
    e.store.create_fact("s", &ent.id, "note", None, "do not send me", false).await.unwrap();
    e.mode("s", "Enforce", None).await;
    let sent = e.send("/v1/messages", "s", &body).await;
    assert_eq!(sent, body, "extracted and unmarked facts change nothing");
    assert!(!sent.to_string().contains("loom notes"));
}

#[tokio::test]
async fn edited_text_replaces_the_node_in_enforce() {
    let e = env().await;
    let body = anthropic();
    start(&e, "/v1/messages", "s", &body).await;
    let id = node_with(&e, "s", NodeKind::ToolResult).await;
    e.store.set_override(&id, "fn parse() { /* the user's corrected body */ }").await.unwrap();
    e.mode("s", "Enforce", None).await;

    let sent = e.send("/v1/messages", "s", &body).await;
    let s = sent.to_string();
    assert!(s.contains("the user's corrected body") && !s.contains("the original body"));
    // the tool pair is intact and nothing else moved
    assert_eq!(sent["messages"][2]["content"][0]["tool_use_id"], "t1");
    assert_eq!(sent["messages"][1], body["messages"][1]);
    assert_eq!(sent["messages"][4], body["messages"][4]);
    // the original text is still in the store, and clearing the edit restores the agent's request exactly
    let detail = e.store.node_detail(&id).await.unwrap().unwrap();
    assert!(detail.node.content.contains("the original body"));
    e.store.clear_override(&id).await.unwrap();
    assert_eq!(e.send("/v1/messages", "s", &body).await, body);
}

#[tokio::test]
async fn nothing_is_applied_outside_enforce() {
    for mode in ["Observe", "Suggest"] {
        let e = env().await;
        let body = anthropic();
        start(&e, "/v1/messages", "s", &body).await;
        let id = node_with(&e, "s", NodeKind::ToolResult).await;
        e.store.set_override(&id, "EDITED").await.unwrap();
        add_note(&e, "s", "parser", "a note").await;
        e.mode("s", mode, None).await;
        assert_eq!(e.send("/v1/messages", "s", &body).await, body, "{mode} forwards the original request");
    }
}

#[tokio::test]
async fn an_edit_on_a_node_the_plan_drops_disappears_with_it() {
    let e = env().await;
    let mut body = anthropic();
    // make the tool result big and old so the tiny budget drops the pair
    body["messages"][2]["content"][0]["content"] = json!("noise line\n".repeat(2000));
    // More conversation after it, so the pair is old enough to fall outside the protected recent context.
    for i in 0..3 {
        body["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "assistant", "content": format!("step {i} done")}));
        body["messages"].as_array_mut().unwrap().push(json!({"role": "user", "content": format!("continue {i}")}));
    }
    start(&e, "/v1/messages", "s", &body).await;
    let id = node_with(&e, "s", NodeKind::ToolResult).await;
    e.store.set_override(&id, "EDITED RESULT").await.unwrap();
    e.mode("s", "Enforce", Some(60)).await;
    let sent = e.send("/v1/messages", "s", &body).await;
    let s = sent.to_string();
    assert!(!s.contains("EDITED RESULT") && !s.contains("noise line"), "dropped nodes take their edits with them");
    assert!(!s.contains("\"t1\""));
}

#[tokio::test]
async fn the_notes_come_out_of_the_budget() {
    let e = env().await;
    let body = anthropic();
    start(&e, "/v1/messages", "s", &body).await;
    e.mode("s", "Enforce", Some(100_000)).await;
    add_note(&e, "s", "parser", &"important detail. ".repeat(100)).await;
    let before = e.store.policy_data("s").await.unwrap().unwrap();
    assert!(before.memory_tokens > 100);
    assert_eq!(before.input.budget, 100_000 - before.memory_tokens);
}

#[tokio::test]
async fn openai_requests_get_edits_and_notes_too() {
    let e = env().await;
    let body = openai();
    start(&e, "/v1/chat/completions", "o", &body).await;
    let id = node_with(&e, "o", NodeKind::ToolResult).await;
    e.store.set_override(&id, "corrected output").await.unwrap();
    add_note(&e, "o", "parser", "parse() must never panic").await;
    e.mode("o", "Enforce", None).await;

    let sent = e.send("/v1/chat/completions", "o", &body).await;
    let msgs = sent["messages"].as_array().unwrap();
    assert_eq!(msgs[3]["content"], "corrected output");
    assert_eq!(msgs[3]["tool_call_id"], "c1");
    // the request ended with tool output, so the notes are a new user message after it
    assert_eq!(msgs.len(), 5);
    assert_eq!(msgs[4]["role"], "user");
    assert!(msgs[4]["content"].as_str().unwrap().contains("parse() must never panic"));
    loom_core::rewrite::validate_request(loom_core::model::Provider::OpenAi, &sent).unwrap();
}

#[tokio::test]
async fn notes_are_not_fed_back_into_the_agents_history() {
    // Loom records the agent's own request, never the request it rewrote, so notes cannot snowball.
    let e = env().await;
    let body = anthropic();
    start(&e, "/v1/messages", "s", &body).await;
    add_note(&e, "s", "parser", "parse() must never panic").await;
    e.mode("s", "Enforce", None).await;
    for _ in 0..3 {
        e.send("/v1/messages", "s", &body).await;
    }
    let nodes = e.store.list_nodes("s").await.unwrap();
    assert!(nodes.iter().all(|n| !n.content.contains("loom notes")));
    assert_eq!(nodes.iter().filter(|n| n.kind == NodeKind::User).count(), 2);
}
