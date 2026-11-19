use std::{sync::Arc, time::Duration};

use axum::Router;
use loom_server::gateway;
use serde_json::{json, Value};

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

struct Env {
    proxy: String,
    c: reqwest::Client,
    store: Arc<loom_store::Store>,
}

async fn env() -> Env {
    let upstream = serve(Router::new().fallback(|| async { "{}" })).await;
    let store = Arc::new(loom_store::Store::open(":memory:").await.unwrap());
    let state = gateway::State::new(upstream.clone(), upstream).with_store(store.clone());
    Env { proxy: serve(gateway::router(Arc::new(state))).await, c: reqwest::Client::new(), store }
}

impl Env {
    async fn send(&self, session: &str, msgs: Value) {
        let before = self.store.turn_count(session).await.unwrap();
        self.c
            .post(format!("{}/v1/messages", self.proxy))
            .header("x-loom-session", session)
            .body(json!({"model": "claude-x", "system": "be brief", "messages": msgs}).to_string())
            .send()
            .await
            .unwrap();
        // Ingest runs in the background; wait for this request to land.
        for _ in 0..200 {
            if self.store.turn_count(session).await.unwrap() > before {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("request was not ingested");
    }
    async fn get(&self, path: &str) -> Value {
        self.c.get(format!("{}{path}", self.proxy)).send().await.unwrap().json().await.unwrap()
    }
}

/// A conversation with one huge, old, irrelevant tool result.
fn transcript() -> Value {
    let mut m = vec![json!({"role": "user", "content": "fix the parser bug"})];
    m.push(json!({"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"cmd": "cat build.log"}}]}));
    m.push(json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "noise ".repeat(4000)}]}));
    for i in 0..6 {
        m.push(json!({"role": if i % 2 == 0 {"assistant"} else {"user"}, "content": format!("step {i} on the parser")}));
    }
    json!(m)
}

#[tokio::test]
async fn plan_reflects_budget_and_user_controls() {
    let e = env().await;
    e.send("p", transcript()).await;
    e.send("p", transcript()).await; // second turn so the first is a stable prefix

    // Roomy default budget: everything is kept.
    let p = e.get("/api/sessions/p/plan").await;
    assert_eq!(p["budget_is_default"], true);
    assert!(p["plan"]["items"].as_array().unwrap().iter().all(|i| i["decision"] == "Keep"));

    // Squeeze the budget: the huge tool pair must go, together.
    let r = e.c.put(format!("{}/api/sessions/p", e.proxy)).json(&json!({"budget_tokens": 400, "mode": "Suggest"})).send().await.unwrap();
    assert_eq!(r.status(), 204);
    let p = e.get("/api/sessions/p/plan").await;
    assert_eq!(p["mode"], "Suggest");
    assert_eq!(p["pending_changes"], true);
    let items = p["plan"]["items"].as_array().unwrap();
    let result = items.iter().find(|i| i["tokens"].as_u64().unwrap() > 1000).unwrap();
    let call = &items[items.iter().position(|i| i["id"] == result["id"]).unwrap() - 1];
    assert_eq!(result["decision"], "Drop");
    assert_eq!(call["decision"], "Drop", "tool pair stays together");
    assert!(p["plan"]["planned_tokens"].as_u64().unwrap() <= 400);
    assert!(result["reason"].as_str().unwrap().starts_with("Dropped"));

    // Pinning the result forces it (and its call) back in, over budget.
    let id = result["id"].as_str().unwrap();
    let r = e.c.patch(format!("{}/api/nodes/{id}", e.proxy)).json(&json!({"pinned": true})).send().await.unwrap();
    assert_eq!(r.status(), 204);
    let p = e.get("/api/sessions/p/plan").await;
    assert_eq!(p["plan"]["over_budget"], true);
    let pinned = p["plan"]["items"].as_array().unwrap().iter().find(|i| i["id"] == id).unwrap().clone();
    assert_eq!(pinned["decision"], "Keep");
    assert!(pinned["reason"].as_str().unwrap().contains("pinned"));

    // Unpin, then force-drop via user action; clearing the override with null works.
    e.c.patch(format!("{}/api/nodes/{id}", e.proxy)).json(&json!({"pinned": false, "user_action": "Keep"})).send().await.unwrap();
    let p = e.get("/api/sessions/p/plan").await;
    assert_eq!(p["plan"]["items"].as_array().unwrap().iter().find(|i| i["id"] == id).unwrap()["decision"], "Keep");
    e.c.patch(format!("{}/api/nodes/{id}", e.proxy)).json(&json!({"user_action": null})).send().await.unwrap();
    let p = e.get("/api/sessions/p/plan").await;
    assert_eq!(p["plan"]["items"].as_array().unwrap().iter().find(|i| i["id"] == id).unwrap()["decision"], "Drop");

    // Restore = keep again.
    assert_eq!(e.c.post(format!("{}/api/nodes/{id}/restore", e.proxy)).send().await.unwrap().status(), 204);
    let p = e.get("/api/sessions/p/plan").await;
    assert_eq!(p["plan"]["items"].as_array().unwrap().iter().find(|i| i["id"] == id).unwrap()["decision"], "Keep");
}

#[tokio::test]
async fn rules_crud_match_count_and_application() {
    let e = env().await;
    e.send("r", transcript()).await;

    let m = json!({"kind": "ToolResult", "min_tokens": 1000});
    let hit: Value = e.c.post(format!("{}/api/sessions/r/match", e.proxy)).json(&m).send().await.unwrap().json().await.unwrap();
    assert_eq!(hit["count"], 1);

    // Empty matcher is rejected.
    let r = e.c.post(format!("{}/api/rules", e.proxy)).json(&json!({"matcher": {}, "action": "Drop"})).send().await.unwrap();
    assert_eq!(r.status(), 400);

    let r: Value = e
        .c
        .post(format!("{}/api/rules", e.proxy))
        .json(&json!({"matcher": m, "action": "Summarize", "priority": 2}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rule_id = r["id"].as_str().unwrap().to_string();
    assert_eq!(e.get("/api/rules?session=r").await.as_array().unwrap().len(), 1);

    let p = e.get("/api/sessions/r/plan").await;
    let items = p["plan"]["items"].as_array().unwrap();
    let big = items.iter().find(|i| i["tokens"].as_u64().unwrap() > 1000).unwrap();
    assert_eq!(big["decision"], "Summarize");
    assert!(big["reason"].as_str().unwrap().contains("rule"));

    assert_eq!(e.c.delete(format!("{}/api/rules/{rule_id}", e.proxy)).send().await.unwrap().status(), 204);
    assert_eq!(e.c.delete(format!("{}/api/rules/{rule_id}", e.proxy)).send().await.unwrap().status(), 404);
    let p = e.get("/api/sessions/r/plan").await;
    assert!(p["plan"]["items"].as_array().unwrap().iter().all(|i| i["decision"] == "Keep"));
}

#[tokio::test]
async fn validation_and_unknowns() {
    let e = env().await;
    e.send("v", transcript()).await;
    let put = |body: Value| e.c.put(format!("{}/api/sessions/v", e.proxy)).json(&body).send();
        assert_eq!(put(json!({"mode": "Nope"})).await.unwrap().status(), 400);
    assert_eq!(put(json!({"budget_tokens": -5})).await.unwrap().status(), 400);
    assert_eq!(put(json!({"weights": {"tau": 0.0}})).await.unwrap().status(), 400);
    assert_eq!(e.c.get(format!("{}/api/sessions/nope/plan", e.proxy)).send().await.unwrap().status(), 404);
    assert_eq!(e.c.patch(format!("{}/api/nodes/nope", e.proxy)).json(&json!({"pinned": true})).send().await.unwrap().status(), 404);
    // Partial weights override only what is given.
    assert_eq!(put(json!({"weights": {"protect_tail": 2}})).await.unwrap().status(), 204);
    let p = e.get("/api/sessions/v/plan").await;
    assert_eq!(p["weights"]["protect_tail"], 2);
    assert_eq!(p["weights"]["w_imp"], 1.0);
}
