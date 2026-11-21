use loom_core::{
    model::{EntityKind, NodeKind, Provider},
    normalize::normalize,
};
use loom_store::{EntityPatch, FactPatch, KgError, Store};
use serde_json::{json, Value};

async fn ingest(store: &Store, sid: &str, msgs: &[Value]) {
    let body = json!({"model": "m", "system": "sys", "messages": msgs}).to_string();
    let n = normalize(Provider::Anthropic, body.as_bytes()).unwrap();
    store.ingest(sid, Provider::Anthropic, n.model.as_deref(), &n.msgs).await.unwrap();
}

fn call(id: &str, name: &str, input: Value) -> Value {
    json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]})
}

fn result(id: &str, content: &str) -> Value {
    json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": content}]})
}

/// task, a failing command, a file read, an edit, a passing command.
fn transcript() -> Vec<Value> {
    vec![
        json!({"role": "user", "content": "Run the tests and fix the failing one"}),
        call("t1", "run_command", json!({"command": "cargo test"})),
        result("t1", "exit code 101\nerror: could not find `Cargo.toml` in this directory"),
        call("t2", "read_file", json!({"path": "src/math.js"})),
        result("t2", "function add(a, b) {\n  return a + b;\n}\nfunction average(numbers) {\n  return 0;\n}\n"),
        call("t3", "edit_file", json!({"path": "src/math.js", "old": "return 0;", "new": "return 1;"})),
        result("t3", "edited src/math.js"),
        call("t4", "run_command", json!({"command": "npm test"})),
        result("t4", "exit code 0\nall tests passed"),
    ]
}

async fn store_with_session() -> Store {
    let store = Store::open(":memory:").await.unwrap();
    ingest(&store, "s", &transcript()).await;
    store
}

fn entity<'a>(k: &'a loom_store::Knowledge, kind: EntityKind, name: &str) -> &'a loom_store::Entity {
    k.entities.iter().find(|e| e.kind == kind && e.name == name).unwrap_or_else(|| {
        panic!("no {kind:?} '{name}' in {:?}", k.entities.iter().map(|e| &e.name).collect::<Vec<_>>())
    })
}

#[tokio::test]
async fn ingest_extracts_entities_and_facts() {
    let store = store_with_session().await;
    let k = store.knowledge("s").await.unwrap();

    let goal = entity(&k, EntityKind::Goal, "Run the tests and fix the failing one");
    assert_eq!(goal.source, "auto");
    let file = entity(&k, EntityKind::File, "src/math.js");
    let cargo = entity(&k, EntityKind::Command, "cargo test");
    let npm = entity(&k, EntityKind::Command, "npm test");
    entity(&k, EntityKind::Symbol, "add");
    entity(&k, EntityKind::Symbol, "average");
    assert!(k.entities.iter().any(|e| e.kind == EntityKind::Error && e.name.contains("could not find")));

    let facts_of =
        |id: &str| k.facts.iter().filter(|f| f.subject_id == id).map(|f| f.predicate.as_str()).collect::<Vec<_>>();
    assert_eq!(facts_of(&goal.id), ["task"]);
    assert!(facts_of(&file.id).contains(&"edited") && facts_of(&file.id).contains(&"defines"));
    assert_eq!(facts_of(&cargo.id), ["result", "failed_with"]);
    assert_eq!(facts_of(&npm.id), ["result"]);
    let edit = k.facts.iter().find(|f| f.predicate == "edited").unwrap();
    assert_eq!(edit.text, "Edited: `return 0;` → `return 1;`");
    assert!(edit.node_id.is_some(), "every fact points at the node it came from");
    assert!(k.facts.iter().all(|f| !f.inject && f.source == "auto"), "nothing is sent to the model until you say so");
}

#[tokio::test]
async fn re_ingesting_does_not_duplicate_and_new_turns_add_only_new_facts() {
    let store = store_with_session().await;
    let before = store.knowledge("s").await.unwrap();
    ingest(&store, "s", &transcript()).await;
    let again = store.knowledge("s").await.unwrap();
    assert_eq!((again.entities.len(), again.facts.len()), (before.entities.len(), before.facts.len()));

    let mut longer = transcript();
    longer.push(call("t5", "run_command", json!({"command": "git status"})));
    longer.push(result("t5", "exit code 0\nnothing to commit"));
    ingest(&store, "s", &longer).await;
    let k = store.knowledge("s").await.unwrap();
    assert_eq!(k.entities.len(), before.entities.len() + 1);
    assert_eq!(k.facts.len(), before.facts.len() + 1);
    entity(&k, EntityKind::Command, "git status");
}

#[tokio::test]
async fn sessions_have_separate_graphs() {
    let store = store_with_session().await;
    ingest(&store, "other", &[json!({"role": "user", "content": "something else"})]).await;
    assert!(store.knowledge("other").await.unwrap().entities.iter().all(|e| e.kind == EntityKind::Goal));
    assert!(store.knowledge("s").await.unwrap().entities.len() > 5);
}

#[tokio::test]
async fn entities_can_be_created_renamed_and_deleted_with_their_facts() {
    let store = store_with_session().await;
    let made =
        store.create_entity("s", EntityKind::Concept, "  rounding rules  ", Some("round half up")).await.unwrap();
    assert_eq!(
        (made.name.as_str(), made.source.as_str(), made.note.as_deref()),
        ("rounding rules", "user", Some("round half up"))
    );

    // duplicates are refused, empty and oversized names are refused, unknown sessions are refused
    assert!(matches!(
        store.create_entity("s", EntityKind::Concept, "rounding rules", None).await,
        Err(KgError::Conflict(_))
    ));
    assert!(matches!(store.create_entity("s", EntityKind::Concept, "   ", None).await, Err(KgError::Invalid(_))));
    assert!(matches!(
        store.create_entity("s", EntityKind::Concept, &"x".repeat(201), None).await,
        Err(KgError::Invalid(_))
    ));
    assert!(matches!(store.create_entity("nope", EntityKind::Concept, "a", None).await, Err(KgError::NotFound(_))));

    let renamed = store
        .update_entity(
            &made.id,
            &EntityPatch { name: Some("rounding".into()), kind: Some(EntityKind::Note), note: Some(None) },
        )
        .await
        .unwrap();
    assert_eq!((renamed.name.as_str(), renamed.kind, renamed.note), ("rounding", EntityKind::Note, None));

    // renaming onto an existing entity is a conflict
    let other = store.create_entity("s", EntityKind::Note, "other", None).await.unwrap();
    let clash =
        store.update_entity(&other.id, &EntityPatch { name: Some("rounding".into()), ..Default::default() }).await;
    assert!(matches!(clash, Err(KgError::Conflict(_))));

    let fact = store.create_fact("s", &renamed.id, "relates_to", Some(&other.id), "", false).await.unwrap();
    store.delete_entity(&renamed.id).await.unwrap();
    let k = store.knowledge("s").await.unwrap();
    assert!(k.entities.iter().all(|e| e.id != renamed.id));
    assert!(k.facts.iter().all(|f| f.id != fact.id), "facts of a deleted entity go with it");
    assert!(matches!(store.get_entity(&renamed.id).await, Err(KgError::NotFound(_))));
}

#[tokio::test]
async fn deleted_extracted_things_do_not_come_back() {
    let store = store_with_session().await;
    let k = store.knowledge("s").await.unwrap();
    let cargo = entity(&k, EntityKind::Command, "cargo test").clone();
    let edit = k.facts.iter().find(|f| f.predicate == "edited").unwrap().clone();

    store.delete_entity(&cargo.id).await.unwrap();
    store.delete_fact(&edit.id).await.unwrap();

    // The agent sends the same history again, plus a new turn: the deleted things stay deleted.
    let mut longer = transcript();
    longer.push(call("t5", "run_command", json!({"command": "cargo test"})));
    longer.push(result("t5", "exit code 101\nerror: still no manifest"));
    ingest(&store, "s", &longer).await;
    let k = store.knowledge("s").await.unwrap();
    assert!(k.entities.iter().all(|e| e.name != "cargo test"));
    assert!(k.facts.iter().all(|f| f.predicate != "edited"));

    // You can add it back yourself.
    let again = store.create_entity("s", EntityKind::Command, "cargo test", None).await.unwrap();
    assert_eq!(again.source, "user");
    assert!(store.knowledge("s").await.unwrap().entities.iter().any(|e| e.name == "cargo test"));
}

#[tokio::test]
async fn facts_can_be_created_edited_and_deleted() {
    let store = store_with_session().await;
    let k = store.knowledge("s").await.unwrap();
    let file = entity(&k, EntityKind::File, "src/math.js").clone();
    let avg = entity(&k, EntityKind::Symbol, "average").clone();

    let f = store.create_fact("s", &avg.id, "has_bug", None, "divided by n+1", true).await.unwrap();
    assert_eq!((f.source.as_str(), f.inject, f.text.as_str()), ("user", true, "divided by n+1"));

    let linked = store.create_fact("s", &file.id, "contains", Some(&avg.id), "", false).await.unwrap();
    assert_eq!(linked.object_id.as_deref(), Some(avg.id.as_str()));

    assert!(matches!(store.create_fact("s", &avg.id, "", None, "", false).await, Err(KgError::Invalid(_))));
    assert!(matches!(store.create_fact("s", "missing", "x", None, "", false).await, Err(KgError::NotFound(_))));
    assert!(matches!(
        store.create_fact("s", &avg.id, "x", None, &"y".repeat(2001), false).await,
        Err(KgError::Invalid(_))
    ));
    // an entity from another session cannot be used
    ingest(&store, "other", &[json!({"role": "user", "content": "elsewhere"})]).await;
    let foreign = store.knowledge("other").await.unwrap().entities[0].id.clone();
    assert!(matches!(store.create_fact("s", &avg.id, "x", Some(&foreign), "", false).await, Err(KgError::Invalid(_))));

    // toggling "send to the model" keeps an extracted fact extracted; changing what it says makes it yours
    let auto = k.facts.iter().find(|f| f.predicate == "edited").unwrap().clone();
    let toggled = store.update_fact(&auto.id, &FactPatch { inject: Some(true), ..Default::default() }).await.unwrap();
    assert_eq!((toggled.source.as_str(), toggled.inject), ("auto", true));
    let edited = store
        .update_fact(&auto.id, &FactPatch { text: Some("Fixed the average".into()), ..Default::default() })
        .await
        .unwrap();
    assert_eq!((edited.source.as_str(), edited.text.as_str(), edited.inject), ("user", "Fixed the average", true));
    store
        .update_fact(&auto.id, &FactPatch { object_id: Some(Some(avg.id.clone())), ..Default::default() })
        .await
        .unwrap();
    store.update_fact(&auto.id, &FactPatch { object_id: Some(None), ..Default::default() }).await.unwrap();

    // your own facts are removed for good; extracted ones are kept as tombstones
    store.delete_fact(&f.id).await.unwrap();
    assert!(matches!(store.get_fact(&f.id).await, Err(KgError::NotFound(_))));
    let defines = k.facts.iter().find(|f| f.predicate == "defines").unwrap().clone();
    store.delete_fact(&defines.id).await.unwrap();
    ingest(&store, "s", &transcript()).await;
    assert!(store.knowledge("s").await.unwrap().facts.iter().all(|f| f.id != defines.id));
}

#[tokio::test]
async fn injected_facts_are_the_ones_you_marked_in_creation_order() {
    let store = store_with_session().await;
    let k = store.knowledge("s").await.unwrap();
    let avg = entity(&k, EntityKind::Symbol, "average").clone();
    let file = entity(&k, EntityKind::File, "src/math.js").clone();
    assert!(store.injected_facts("s").await.unwrap().is_empty());

    store.create_fact("s", &avg.id, "has_bug", None, "divides by n+1", true).await.unwrap();
    store.create_fact("s", &file.id, "contains", Some(&avg.id), "", true).await.unwrap();
    store.create_fact("s", &avg.id, "private", None, "not sent", false).await.unwrap();
    let inj = store.injected_facts("s").await.unwrap();
    assert_eq!(inj.len(), 2);
    assert_eq!(
        (inj[0].subject.as_str(), inj[0].predicate.as_str(), inj[0].text.as_str()),
        ("average", "has_bug", "divides by n+1")
    );
    assert_eq!((inj[1].subject.as_str(), inj[1].object.as_deref()), ("src/math.js", Some("average")));
    // deleting an entity removes its facts from what is sent
    store.delete_entity(&avg.id).await.unwrap();
    assert!(store.injected_facts("s").await.unwrap().is_empty());
}

#[tokio::test]
async fn node_text_can_be_overridden_and_reverted() {
    let store = store_with_session().await;
    let nodes = store.list_nodes("s").await.unwrap();
    let user = nodes.iter().find(|n| n.kind == NodeKind::User).unwrap();
    let call = nodes.iter().find(|n| n.kind == NodeKind::ToolCall).unwrap();
    let system = nodes.iter().find(|n| n.kind == NodeKind::System).unwrap();

    assert!(!user.edited);
    store.set_override(&user.id, "Run the tests, fix the failing one, and explain the cause").await.unwrap();
    let d = store.node_detail(&user.id).await.unwrap().unwrap();
    assert_eq!(d.override_text.as_deref(), Some("Run the tests, fix the failing one, and explain the cause"));
    assert_eq!(d.node.content, "Run the tests and fix the failing one", "the original is never lost");
    assert!(d.node.edited);
    assert!(store.list_nodes("s").await.unwrap().iter().find(|n| n.id == user.id).unwrap().edited);

    // calls, the system prompt and empty text cannot be edited
    for id in [&call.id, &system.id] {
        assert!(matches!(store.set_override(id, "x").await, Err(KgError::Invalid(_))));
    }
    assert!(matches!(store.set_override(&user.id, "   ").await, Err(KgError::Invalid(_))));
    assert!(matches!(store.set_override("nope", "x").await, Err(KgError::NotFound(_))));

    store.clear_override(&user.id).await.unwrap();
    assert!(!store.node_detail(&user.id).await.unwrap().unwrap().node.edited);
}

#[tokio::test]
async fn the_plan_budgets_for_edits_and_notes() {
    let store = store_with_session().await;
    store
        .update_session("s", &loom_store::SessionUpdate { budget_tokens: Some(10_000), ..Default::default() })
        .await
        .unwrap();
    let base = store.policy_data("s").await.unwrap().unwrap();
    assert_eq!((base.memory.is_none(), base.memory_tokens, base.input.budget), (true, 0, 10_000));
    assert!(base.overrides.is_empty() && base.input.no_summary.is_empty());

    let nodes = store.list_nodes("s").await.unwrap();
    let result_node = nodes.iter().find(|n| n.kind == NodeKind::File).unwrap();
    store.set_override(&result_node.id, "short").await.unwrap();
    let k = store.knowledge("s").await.unwrap();
    let avg = entity(&k, EntityKind::Symbol, "average").clone();
    store.create_fact("s", &avg.id, "has_bug", None, "divides by n+1", true).await.unwrap();

    let d = store.policy_data("s").await.unwrap().unwrap();
    let pn = d.input.nodes.iter().find(|n| n.id == result_node.id).unwrap();
    assert_eq!(pn.content, "short");
    assert!(pn.tokens < result_node.tokens, "the edited text is smaller, so it costs less");
    assert!(d.input.no_summary.contains(&result_node.id), "edited text is never replaced by a summary");
    assert_eq!(d.overrides.get(&result_node.id).map(String::as_str), Some("short"));
    let memory = d.memory.as_deref().unwrap();
    assert!(memory.starts_with("[loom notes]") && memory.contains("average: divides by n+1"));
    assert!(d.memory_tokens > 0);
    assert_eq!(d.input.budget, 10_000 - d.memory_tokens, "the notes take their share of the budget");

    // The same through the request-specific loader used by Enforce.
    let body = json!({"model": "m", "system": "sys", "messages": transcript()}).to_string();
    let n = normalize(Provider::Anthropic, body.as_bytes()).unwrap();
    let d2 = store.policy_data_for_request("s", &n.msgs).await.unwrap().unwrap();
    assert_eq!(d2.input.nodes.iter().find(|n| n.id == result_node.id).unwrap().content, "short");
}
