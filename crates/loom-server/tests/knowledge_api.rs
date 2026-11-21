use std::{sync::Arc, time::Duration};

use axum::Router;
use loom_server::gateway;
use reqwest::StatusCode;
use serde_json::{json, Value};

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

struct Env {
    base: String,
    c: reqwest::Client,
    store: Arc<loom_store::Store>,
    events: tokio::sync::broadcast::Receiver<gateway::Event>,
}

async fn env() -> Env {
    let upstream = serve(Router::new().fallback(|| async { "{}" })).await;
    let store = Arc::new(loom_store::Store::open(":memory:").await.unwrap());
    let state = Arc::new(gateway::State::new(upstream.clone(), upstream).with_store(store.clone()));
    let events = state.events.subscribe();
    Env { base: serve(gateway::router(state)).await, c: reqwest::Client::new(), store, events }
}

impl Env {
    async fn seed(&self, session: &str) {
        let body = json!({"model": "m", "system": "s", "messages": [
            {"role": "user", "content": "Run the tests and fix the failing one"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "read_file", "input": {"path": "src/math.js"}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "function add(a, b) {\n  return a + b;\n}\nfunction average(n) {\n  return 0;\n}\n"}]},
        ]});
        self.c
            .post(format!("{}/v1/messages", self.base))
            .header("x-loom-session", session)
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        for _ in 0..200 {
            if self.store.turn_count(session).await.unwrap() > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("not ingested");
    }

    async fn get(&self, path: &str) -> Value {
        self.c.get(format!("{}{path}", self.base)).send().await.unwrap().json().await.unwrap()
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (StatusCode, String) {
        let mut r = self.c.request(method, format!("{}{path}", self.base));
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        (resp.status(), resp.text().await.unwrap())
    }

    fn drain_events(&mut self) -> Vec<&'static str> {
        let mut out = vec![];
        while let Ok(e) = self.events.try_recv() {
            out.push(e.kind);
        }
        out
    }
}

async fn post(e: &Env, path: &str, body: Value) -> (StatusCode, String) {
    e.send(reqwest::Method::POST, path, Some(body)).await
}

fn find<'a>(k: &'a Value, kind: &str, name: &str) -> &'a Value {
    k["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == kind && e["name"] == name)
        .unwrap_or_else(|| panic!("no {kind} {name}"))
}

#[tokio::test]
async fn the_graph_is_served_with_extracted_entities_and_facts() {
    let e = env().await;
    e.seed("s").await;
    let k = e.get("/api/sessions/s/knowledge").await;
    let file = find(&k, "File", "src/math.js");
    assert_eq!(file["source"], "auto");
    find(&k, "Symbol", "average");
    assert!(k["facts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["subject_id"] == file["id"] && f["predicate"] == "defines"));
    assert_eq!(k["memory"], Value::Null, "nothing is sent until you mark a fact");
    assert_eq!(k["edited_nodes"], json!([]));
    let (status, _) = e.send(reqwest::Method::GET, "/api/sessions/nope/knowledge", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn entities_and_facts_can_be_created_edited_and_deleted_over_http() {
    let mut e = env().await;
    e.seed("s").await;
    e.drain_events();

    // create an entity
    let (st, body) = e
        .send(
            reqwest::Method::POST,
            "/api/sessions/s/knowledge/entities",
            Some(json!({"kind": "Concept", "name": "rounding", "note": "half up"})),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let ent: Value = serde_json::from_str(&body).unwrap();
    assert_eq!((ent["source"].as_str(), ent["note"].as_str()), (Some("user"), Some("half up")));
    assert!(e.drain_events().contains(&"knowledge_updated"));

    // validation and conflicts
    assert_eq!(
        post(&e, "/api/sessions/s/knowledge/entities", json!({"kind": "Concept", "name": "rounding"})).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        post(&e, "/api/sessions/s/knowledge/entities", json!({"kind": "Concept", "name": "  "})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(&e, "/api/sessions/nope/knowledge/entities", json!({"kind": "Concept", "name": "x"})).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(post(&e, "/api/sessions/s/knowledge/entities", json!({"kind": "Nonsense", "name": "x"}))
        .await
        .0
        .is_client_error());

    // a fact linking it to an extracted symbol
    let k = e.get("/api/sessions/s/knowledge").await;
    let avg = find(&k, "Symbol", "average")["id"].as_str().unwrap().to_string();
    let (st, body) = e
        .send(reqwest::Method::POST, "/api/sessions/s/knowledge/facts", Some(json!({"subject_id": avg, "predicate": "depends_on", "object_id": ent["id"], "text": "uses rounding", "inject": true})))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let fact: Value = serde_json::from_str(&body).unwrap();
    let fid = fact["id"].as_str().unwrap();
    assert_eq!((fact["source"].as_str(), fact["inject"].as_bool()), (Some("user"), Some(true)));
    assert_eq!(
        e.send(
            reqwest::Method::POST,
            "/api/sessions/s/knowledge/facts",
            Some(json!({"subject_id": "missing", "predicate": "x"}))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        e.send(
            reqwest::Method::POST,
            "/api/sessions/s/knowledge/facts",
            Some(json!({"subject_id": avg, "predicate": ""}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    // the preview shows exactly what would be sent, with its token cost
    let k = e.get("/api/sessions/s/knowledge").await;
    assert_eq!(k["memory"]["facts"], 1);
    assert!(k["memory"]["tokens"].as_u64().unwrap() > 0);
    assert!(k["memory"]["text"].as_str().unwrap().contains("average depends_on rounding: uses rounding"));

    // edit and unmark
    let (st, body) = e
        .send(
            reqwest::Method::PATCH,
            &format!("/api/knowledge/facts/{fid}"),
            Some(json!({"text": "rounds half up", "inject": false})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["text"], "rounds half up");
    assert_eq!(e.get("/api/sessions/s/knowledge").await["memory"], Value::Null);

    // rename the entity, then delete it (its facts go with it)
    let (st, _) = e
        .send(
            reqwest::Method::PATCH,
            &format!("/api/knowledge/entities/{}", ent["id"].as_str().unwrap()),
            Some(json!({"name": "rounding mode", "note": null})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = e
        .send(reqwest::Method::DELETE, &format!("/api/knowledge/entities/{}", ent["id"].as_str().unwrap()), None)
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let k = e.get("/api/sessions/s/knowledge").await;
    assert!(k["entities"].as_array().unwrap().iter().all(|x| x["id"] != ent["id"]));
    assert!(k["facts"].as_array().unwrap().iter().all(|f| f["id"] != fact["id"]));
    assert_eq!(
        e.send(reqwest::Method::DELETE, &format!("/api/knowledge/facts/{fid}"), None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn extracted_facts_can_be_deleted_and_stay_deleted() {
    let e = env().await;
    e.seed("s").await;
    let k = e.get("/api/sessions/s/knowledge").await;
    let fact = k["facts"].as_array().unwrap().iter().find(|f| f["predicate"] == "defines").unwrap().clone();
    let (st, _) =
        e.send(reqwest::Method::DELETE, &format!("/api/knowledge/facts/{}", fact["id"].as_str().unwrap()), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    e.seed("s").await; // the agent resends the same history
    let k = e.get("/api/sessions/s/knowledge").await;
    assert!(k["facts"].as_array().unwrap().iter().all(|f| f["id"] != fact["id"]));
}

#[tokio::test]
async fn node_text_can_be_read_replaced_and_restored() {
    let mut e = env().await;
    e.seed("s").await;
    let nodes = e.get("/api/sessions/s/nodes").await;
    let result = nodes.as_array().unwrap().iter().find(|n| n["kind"] == "File").unwrap();
    let id = result["id"].as_str().unwrap();
    assert_eq!(result["edited"], false);
    e.drain_events();

    let d = e.get(&format!("/api/nodes/{id}")).await;
    assert!(d["content"].as_str().unwrap().contains("function average"), "the detail has the full text");
    assert_eq!(d["override"], Value::Null);

    let (st, _) = e
        .send(
            reqwest::Method::PUT,
            &format!("/api/nodes/{id}/content"),
            Some(json!({"text": "function average(n) { return sum(n) / n.length; }"})),
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let events = e.drain_events();
    assert!(events.contains(&"knowledge_updated") && events.contains(&"plan_updated"));
    let d = e.get(&format!("/api/nodes/{id}")).await;
    assert_eq!(d["override"], "function average(n) { return sum(n) / n.length; }");
    assert!(d["content"].as_str().unwrap().contains("return 0;"), "the original is kept");
    assert_eq!(d["edited"], true);
    let k = e.get("/api/sessions/s/knowledge").await;
    assert_eq!(k["edited_nodes"], json!([id]));
    assert_eq!(
        e.get("/api/sessions/s/nodes").await.as_array().unwrap().iter().find(|n| n["id"] == id).unwrap()["edited"],
        true
    );

    // the plan reports the edit's effect on size
    let plan = e.get("/api/sessions/s/plan").await;
    assert!(plan["plan"]["original_tokens"].as_u64().unwrap() > 0);

    let (st, _) = e.send(reqwest::Method::DELETE, &format!("/api/nodes/{id}/content"), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert_eq!(e.get(&format!("/api/nodes/{id}")).await["override"], Value::Null);
}

#[tokio::test]
async fn nodes_that_cannot_be_edited_are_refused_clearly() {
    let e = env().await;
    e.seed("s").await;
    let nodes = e.get("/api/sessions/s/nodes").await;
    let by = |kind: &str| {
        nodes.as_array().unwrap().iter().find(|n| n["kind"] == kind).unwrap()["id"].as_str().unwrap().to_string()
    };
    for kind in ["System", "ToolCall"] {
        let (st, body) =
            e.send(reqwest::Method::PUT, &format!("/api/nodes/{}/content", by(kind)), Some(json!({"text": "x"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{kind}");
        assert!(body.contains("cannot be edited"), "{body}");
    }
    let user = by("User");
    assert_eq!(
        e.send(reqwest::Method::PUT, &format!("/api/nodes/{user}/content"), Some(json!({"text": "   "}))).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        e.send(reqwest::Method::PUT, "/api/nodes/nope/content", Some(json!({"text": "x"}))).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(e.send(reqwest::Method::GET, "/api/nodes/nope", None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(e.send(reqwest::Method::DELETE, "/api/nodes/nope/content", None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn existing_node_controls_still_work_next_to_the_new_routes() {
    let e = env().await;
    e.seed("s").await;
    let nodes = e.get("/api/sessions/s/nodes").await;
    let id = nodes.as_array().unwrap()[1]["id"].as_str().unwrap().to_string();
    // PATCH /api/nodes/{id} (pin) shares its path with the new GET /api/nodes/{id}
    let (st, _) = e.send(reqwest::Method::PATCH, &format!("/api/nodes/{id}"), Some(json!({"pinned": true}))).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert_eq!(e.get(&format!("/api/nodes/{id}")).await["pinned"], true);
}

#[tokio::test]
async fn the_plan_reports_what_the_notes_cost() {
    let e = env().await;
    e.seed("s").await;
    assert_eq!(e.get("/api/sessions/s/plan").await["notes_tokens"], 0);
    let k = e.get("/api/sessions/s/knowledge").await;
    let avg = find(&k, "Symbol", "average")["id"].clone();
    e.send(
        reqwest::Method::POST,
        "/api/sessions/s/knowledge/facts",
        Some(json!({"subject_id": avg, "predicate": "has_bug", "text": "returns 0", "inject": true})),
    )
    .await;
    let plan = e.get("/api/sessions/s/plan").await;
    let notes = plan["notes_tokens"].as_u64().unwrap();
    assert!(notes > 0);
    let k = e.get("/api/sessions/s/knowledge").await;
    assert_eq!(k["memory"]["tokens"].as_u64().unwrap(), notes, "the preview and the plan agree");
}
