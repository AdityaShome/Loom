use loom_core::{
    model::{Provider, Status},
    normalize::normalize,
};
use loom_store::Store;
use serde_json::{json, Value};

fn body(msgs: &[Value]) -> Vec<u8> {
    json!({"model": "m", "system": "be brief", "messages": msgs}).to_string().into_bytes()
}

async fn ingest(store: &Store, sid: &str, msgs: &[Value]) -> loom_store::IngestOutcome {
    let n = normalize(Provider::Anthropic, &body(msgs)).unwrap();
    store.ingest(sid, Provider::Anthropic, n.model.as_deref(), &n.msgs).await.unwrap()
}

#[tokio::test]
async fn hundred_turn_session_has_one_node_per_unique_message() {
    let store = Store::open(":memory:").await.unwrap();
    let mut msgs = vec![json!({"role":"user","content":"task"})];
    for t in 0..100 {
        let r = ingest(&store, "s", &msgs).await;
        assert_eq!(r.turn, t);
        // First turn creates system + all messages; later turns add exactly the 2 new messages.
        assert_eq!(r.new, 2, "turn {t}"); // t=0: system + task; later: the 2 new messages
        assert_eq!(r.missing, 0);
        if t % 2 == 0 {
            let id = format!("toolu_{t}");
            msgs.push(json!({"role":"assistant","content":[{"type":"tool_use","id":id,"name":"Bash","input":{"cmd":"ls"}}]}));
            msgs.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":"same output"}]}));
        } else {
            // Identical text repeated every odd turn: must still yield distinct nodes.
            msgs.push(json!({"role":"assistant","content":"done"}));
            msgs.push(json!({"role":"user","content":"ok"}));
        }
    }
    // 1 system + 1 task + 2 per turn for the 99 turns that preceded a later request.
    let nodes = store.list_nodes("s").await.unwrap();
    assert_eq!(nodes.len(), 1 + 1 + 2 * 99);
    assert_eq!(store.turn_count("s").await.unwrap(), 100);
    assert!(nodes.iter().all(|n| n.status == Status::Full));

    // Re-sending the last ingested request creates nothing new. (The loop pushed
    // two more messages after its final ingest, so drop them.)
    let sent = &msgs[..msgs.len() - 2];
    let again = ingest(&store, "s", sent).await;
    assert_eq!(again.new, 0);
    assert_eq!(store.list_nodes("s").await.unwrap().len(), nodes.len());

    // Snapshot of the last turn lists every message sent, in order.
    let ids = store.snapshot_node_ids("s", 100).await.unwrap().unwrap();
    assert_eq!(ids.len(), sent.len() + 1); // + system
}

#[tokio::test]
async fn agent_compaction_is_detected_and_reversible() {
    let store = Store::open(":memory:").await.unwrap();
    let all: Vec<Value> = (0..6)
        .map(|i| json!({"role": if i % 2 == 0 {"user"} else {"assistant"}, "content": format!("m{i}")}))
        .collect();
    ingest(&store, "s", &all).await;

    let compact = [all[0].clone(), all[5].clone()];
    let r = ingest(&store, "s", &compact).await;
    assert_eq!((r.new, r.missing), (0, 4));
    let dropped = store.list_nodes("s").await.unwrap().iter().filter(|n| n.status == Status::DroppedByAgent).count();
    assert_eq!(dropped, 4);

    ingest(&store, "s", &all).await;
    assert!(store.list_nodes("s").await.unwrap().iter().all(|n| n.status == Status::Full));
}

#[tokio::test]
async fn sessions_are_isolated_and_listed() {
    let store = Store::open(":memory:").await.unwrap();
    let m = [json!({"role":"user","content":"hi"})];
    ingest(&store, "a", &m).await;
    ingest(&store, "b", &m).await;
    assert_eq!(store.list_nodes("a").await.unwrap().len(), 2);
    let sessions = store.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().all(|s| s.turns == 1 && s.nodes == 2 && s.model.as_deref() == Some("m")));
}
