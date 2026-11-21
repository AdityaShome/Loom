//! Rebuild a provider request from a plan: drop nodes, replace others with
//! summaries, keep everything else byte-for-byte equivalent. Pure; the result
//! is checked with `validate_request` before anything is sent.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::model::{Action, CanonMsg, Loc, NodeKind, Provider};
use crate::policy::Plan;

/// How a summary appears in the request.
pub fn render_summary(text: &str) -> String {
    format!("[loom summary of 1 item] {}", text.trim())
}

enum Op {
    Drop,
    Replace(String),
}

/// Changes the user asked for beyond the plan: replaced node text, and a block of notes for the model.
#[derive(Debug, Clone, Copy, Default)]
pub struct Extras<'a> {
    /// node id -> the text the model should see instead of the original
    pub overrides: Option<&'a HashMap<String, String>>,
    /// Appended to the last user message of the request.
    pub memory: Option<&'a str>,
}

impl Extras<'_> {
    pub fn is_empty(&self) -> bool {
        self.overrides.is_none_or(HashMap::is_empty) && self.memory.is_none()
    }
}

/// Apply `plan` to `original`. `msgs` are the normalized messages of `original`
/// (same order as `plan.items`); `summaries` maps node id to summary text.
pub fn rewrite(
    provider: Provider,
    original: &Value,
    msgs: &[CanonMsg],
    plan: &Plan,
    summaries: &HashMap<String, String>,
) -> Result<Value, String> {
    rewrite_with(provider, original, msgs, plan, summaries, &Extras::default())
}

/// Like [`rewrite`], plus the user's own edits.
pub fn rewrite_with(
    provider: Provider,
    original: &Value,
    msgs: &[CanonMsg],
    plan: &Plan,
    summaries: &HashMap<String, String>,
    extras: &Extras,
) -> Result<Value, String> {
    if msgs.len() != plan.items.len() {
        return Err(format!("{} messages but {} plan items", msgs.len(), plan.items.len()));
    }
    let mut ops: HashMap<(usize, Option<usize>), Op> = HashMap::new();
    for (m, item) in msgs.iter().zip(&plan.items) {
        let edited = extras.overrides.and_then(|o| o.get(&item.id));
        if item.decision == Action::Keep && edited.is_none() {
            continue;
        }
        if m.opaque && (item.decision != Action::Keep || edited.is_some()) {
            return Err(format!("plan would change an unrewritable block ({})", item.id));
        }
        let Loc::Msg { msg, block } = m.loc else {
            return Err("plan would change the system prompt".into());
        };
        match item.decision {
            Action::Drop => {
                ops.insert((msg, block), Op::Drop);
            }
            Action::Summarize if m.kind == NodeKind::ToolCall => {} // call arguments stay verbatim
            Action::Summarize if edited.is_some() => {
                return Err(format!("edited node {} cannot also be summarized", item.id))
            }
            Action::Summarize => {
                let text = summaries.get(&item.id).ok_or_else(|| format!("no summary for {}", item.id))?;
                ops.insert((msg, block), Op::Replace(render_summary(text)));
            }
            Action::Keep => {
                // The user's own text replaces the node's content as is.
                if m.kind == NodeKind::ToolCall {
                    return Err(format!("tool call {} cannot be edited", item.id));
                }
                ops.insert((msg, block), Op::Replace(edited.expect("checked above").clone()));
            }
        }
    }
    if ops.is_empty() && extras.memory.is_none() {
        return Ok(original.clone());
    }
    let mut out = original.clone();
    let messages = original.get("messages").and_then(Value::as_array).ok_or("no messages array")?;
    let mut new_messages = match provider {
        Provider::Anthropic => anthropic_messages(messages, &ops),
        Provider::OpenAi => openai_messages(messages, &ops),
    };
    if let Some(memory) = extras.memory {
        inject_memory(provider, &mut new_messages, memory);
    }
    out["messages"] = Value::Array(new_messages);
    Ok(out)
}

/// Add `text` to the end of a message's content, whatever shape the content has.
fn append_text(msg: &mut Value, text: &str) {
    match msg.get("content").cloned() {
        Some(Value::String(s)) => {
            msg["content"] = Value::String(format!(
                "{s}

{text}"
            ))
        }
        Some(Value::Array(mut parts)) => {
            parts.push(serde_json::json!({"type": "text", "text": text}));
            msg["content"] = Value::Array(parts);
        }
        _ => msg["content"] = Value::String(text.to_string()),
    }
}

/// Put the notes at the very end of the request, so everything before it (and the provider's prompt cache for it) is
/// untouched. They go into the last user message; if the request ends otherwise, an OpenAI request gets a new user
/// message and an Anthropic request (whose last message may be a prefill) uses its last user message.
fn inject_memory(provider: Provider, messages: &mut Vec<Value>, text: &str) {
    let last_is_user = messages.last().is_some_and(|m| role(m) == "user");
    if last_is_user {
        append_text(messages.last_mut().expect("checked"), text);
        return;
    }
    match provider {
        Provider::OpenAi => messages.push(serde_json::json!({"role": "user", "content": text})),
        Provider::Anthropic => match messages.iter().rposition(|m| role(m) == "user") {
            Some(i) => append_text(&mut messages[i], text),
            None => messages.push(serde_json::json!({"role": "user", "content": text})),
        },
    }
}

fn anthropic_messages(messages: &[Value], ops: &HashMap<(usize, Option<usize>), Op>) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len());
    for (mi, m) in messages.iter().enumerate() {
        let mut m = m.clone();
        match m.get("content").cloned() {
            Some(Value::String(_)) => match ops.get(&(mi, None)) {
                Some(Op::Drop) => continue,
                Some(Op::Replace(t)) => m["content"] = Value::String(t.clone()),
                None => {}
            },
            Some(Value::Array(blocks)) => {
                let mut kept = Vec::with_capacity(blocks.len());
                let mut changed = false;
                for (bi, b) in blocks.into_iter().enumerate() {
                    match ops.get(&(mi, Some(bi))) {
                        Some(Op::Drop) => changed = true,
                        Some(Op::Replace(t)) => {
                            changed = true;
                            let mut b = b;
                            if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                                b["content"] = Value::String(t.clone());
                            } else {
                                b["text"] = Value::String(t.clone());
                            }
                            kept.push(b);
                        }
                        None => kept.push(b),
                    }
                }
                if changed && kept.is_empty() {
                    continue; // nothing left of this message
                }
                m["content"] = Value::Array(kept);
            }
            _ => {}
        }
        out.push(m);
    }
    out
}

fn openai_messages(messages: &[Value], ops: &HashMap<(usize, Option<usize>), Op>) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len());
    for (mi, m) in messages.iter().enumerate() {
        let mut m = m.clone();
        let role = m.get("role").and_then(Value::as_str).unwrap_or("").to_string();
        let content_op = ops.get(&(mi, None));
        let had_tool_calls = m.get("tool_calls").and_then(Value::as_array).is_some_and(|a| !a.is_empty());

        if let Some(tcs) = m.get("tool_calls").and_then(Value::as_array).cloned() {
            let kept: Vec<Value> = tcs
                .into_iter()
                .enumerate()
                .filter(|(k, _)| !matches!(ops.get(&(mi, Some(*k))), Some(Op::Drop)))
                .map(|(_, v)| v)
                .collect();
            if kept.is_empty() {
                m.as_object_mut().map(|o| o.remove("tool_calls"));
            } else {
                m["tool_calls"] = Value::Array(kept);
            }
        }
        let has_tool_calls = m.get("tool_calls").is_some();

        match content_op {
            Some(Op::Replace(t)) => m["content"] = Value::String(t.clone()),
            Some(Op::Drop) => {
                if role == "assistant" && has_tool_calls {
                    m["content"] = Value::Null;
                } else {
                    continue;
                }
            }
            None => {}
        }
        // An assistant message whose tool calls were all dropped and that has no text left.
        let no_text = match m.get("content") {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.trim().is_empty(),
            Some(Value::Array(a)) => a.is_empty(),
            _ => false,
        };
        if role == "assistant" && had_tool_calls && !has_tool_calls && no_text {
            continue;
        }
        out.push(m);
    }
    out
}

/// Structural checks the provider would enforce. A request that fails is never sent.
pub fn validate_request(provider: Provider, body: &Value) -> Result<(), String> {
    let messages = body.get("messages").and_then(Value::as_array).ok_or("no messages array")?;
    if messages.is_empty() {
        return Err("no messages".into());
    }
    match provider {
        Provider::Anthropic => validate_anthropic(messages),
        Provider::OpenAi => validate_openai(messages),
    }
}

fn role(m: &Value) -> &str {
    m.get("role").and_then(Value::as_str).unwrap_or("")
}

fn blocks<'a>(m: &'a Value, ty: &'a str) -> impl Iterator<Item = &'a Value> {
    m.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(move |b| b.get("type").and_then(Value::as_str) == Some(ty))
}

fn validate_anthropic(messages: &[Value]) -> Result<(), String> {
    if role(&messages[0]) != "user" {
        return Err("first message must be a user message".into());
    }
    for (i, m) in messages.iter().enumerate() {
        match m.get("content") {
            Some(Value::String(s)) if !s.trim().is_empty() => {}
            Some(Value::Array(a)) if !a.is_empty() => {}
            _ => return Err(format!("message {i} has empty content")),
        }
        let uses: HashSet<&str> = blocks(m, "tool_use").filter_map(|b| b.get("id").and_then(Value::as_str)).collect();
        let results: HashSet<&str> =
            blocks(m, "tool_result").filter_map(|b| b.get("tool_use_id").and_then(Value::as_str)).collect();
        // Every tool_result answers a tool_use in the message right before it...
        let prev_uses: HashSet<&str> = if i > 0 && role(&messages[i - 1]) == "assistant" {
            blocks(&messages[i - 1], "tool_use").filter_map(|b| b.get("id").and_then(Value::as_str)).collect()
        } else {
            HashSet::new()
        };
        if let Some(bad) = results.iter().find(|r| !prev_uses.contains(*r)) {
            return Err(format!("tool_result {bad} has no matching tool_use in the previous message"));
        }
        // ...and every tool_use is answered in the message right after it.
        if !uses.is_empty() {
            let next = messages.get(i + 1).ok_or("tool_use without a following message")?;
            let answered: HashSet<&str> =
                blocks(next, "tool_result").filter_map(|b| b.get("tool_use_id").and_then(Value::as_str)).collect();
            if let Some(missing) = uses.iter().find(|u| !answered.contains(*u)) {
                return Err(format!("tool_use {missing} has no tool_result in the next message"));
            }
        }
    }
    Ok(())
}

fn validate_openai(messages: &[Value]) -> Result<(), String> {
    if messages.iter().all(|m| matches!(role(m), "system" | "developer")) {
        return Err("no conversation messages".into());
    }
    let mut pending: HashSet<String> = HashSet::new();
    for (i, m) in messages.iter().enumerate() {
        match role(m) {
            "tool" => {
                let id = m.get("tool_call_id").and_then(Value::as_str).unwrap_or("");
                if !pending.remove(id) {
                    return Err(format!("tool message {i} ({id}) has no matching tool_call"));
                }
            }
            r => {
                if !pending.is_empty() {
                    return Err(format!("tool_calls without results before message {i}"));
                }
                if r == "assistant" {
                    pending = m
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|t| t.get("id").and_then(Value::as_str).map(String::from))
                        .collect();
                }
            }
        }
    }
    if pending.is_empty() {
        Ok(())
    } else {
        Err("request ends with unanswered tool_calls".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{build_edges, GNode};
    use crate::matching::{canonical_hash, count_tokens};
    use crate::normalize::normalize;
    use crate::policy::{plan, validate_plan, PNode, PolicyInput, Weights};
    use proptest::prelude::*;
    use serde_json::json;

    /// Run the whole pipeline on a body the way the server does.
    fn pipeline(
        provider: Provider,
        body: &Value,
        budget: u32,
        force: &dyn Fn(usize, &CanonMsg) -> Option<Action>,
    ) -> (Plan, Result<Value, String>, Vec<CanonMsg>) {
        let (p, r, m, _) = pipeline_with(provider, body, budget, force, &|_, _| None, None);
        (p, r, m)
    }

    /// Same, plus the user's own text edits and notes. Returns the overrides that were applied.
    #[allow(clippy::type_complexity)]
    fn pipeline_with(
        provider: Provider,
        body: &Value,
        budget: u32,
        force: &dyn Fn(usize, &CanonMsg) -> Option<Action>,
        edit: &dyn Fn(usize, &CanonMsg) -> Option<String>,
        memory: Option<&str>,
    ) -> (Plan, Result<Value, String>, Vec<CanonMsg>, HashMap<String, String>) {
        let n = normalize(provider, body.to_string().as_bytes()).unwrap();
        let ids: Vec<String> = (0..n.msgs.len()).map(|i| format!("n{i}")).collect();
        let gn: Vec<GNode> = n
            .msgs
            .iter()
            .zip(&ids)
            .map(|(m, id)| GNode {
                id,
                kind: m.kind,
                source: m.source.as_deref(),
                tool_id: m.tool_id.as_deref(),
                text: Some(&m.content),
            })
            .collect();
        let all: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let edges = build_edges(&gn, &all);
        // Only nodes the store would accept an edit for: not the system prompt, tool calls or locked blocks.
        let mut overrides: HashMap<String, String> = HashMap::new();
        for (i, m) in n.msgs.iter().enumerate() {
            if !matches!(m.kind, NodeKind::System | NodeKind::ToolCall | NodeKind::Summary) && !m.opaque {
                if let Some(t) = edit(i, m) {
                    overrides.insert(ids[i].clone(), t);
                }
            }
        }
        let nodes: Vec<PNode> = n
            .msgs
            .iter()
            .zip(&ids)
            .enumerate()
            .map(|(i, (m, id))| {
                let text = overrides.get(id).unwrap_or(&m.content);
                PNode {
                    id: id.clone(),
                    kind: m.kind,
                    content: text.clone(),
                    content_hash: canonical_hash(m),
                    tokens: count_tokens(text) + 4,
                    first_seen_turn: (i / 3) as u32,
                    source: m.source.clone(),
                    importance: 0.5,
                    pinned: false,
                    locked: m.opaque,
                    user_action: force(i, m),
                    summary_tokens: None,
                    prev: Some(Action::Keep),
                }
            })
            .collect();
        let mem_tokens = memory.map(|m| count_tokens(m) + 4).unwrap_or(0);
        let inp = PolicyInput {
            nodes,
            edges,
            latest_turn: (n.msgs.len() / 3) as u32 + 1,
            budget: budget.saturating_sub(mem_tokens),
            weights: Weights { protect_tail: 2, ..Weights::default() },
            rules: vec![],
            provider,
            no_summary: overrides.keys().cloned().collect(),
        };
        let p = plan(&inp);
        assert_eq!(validate_plan(&inp, &p), Ok(()));
        let summaries: HashMap<String, String> =
            p.items.iter().map(|i| (i.id.clone(), format!("short {}", i.id))).collect();
        let extras = Extras { overrides: Some(&overrides), memory };
        let out = rewrite_with(provider, body, &n.msgs, &p, &summaries, &extras);
        (p, out, n.msgs, overrides)
    }

    fn anthropic_body() -> Value {
        json!({"model": "m", "system": "sys", "messages": [
            {"role": "user", "content": "fix the parser"},
            {"role": "assistant", "content": [
                {"type": "text", "text": "reading two files"},
                {"type": "tool_use", "id": "a", "name": "Read", "input": {"file_path": "/r/a.rs"}},
                {"type": "tool_use", "id": "b", "name": "Read", "input": {"file_path": "/r/b.rs"}}]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "a", "content": "fn a() {}
        ".repeat(40)},
                {"type": "tool_result", "tool_use_id": "b", "content": "fn b() {}
        ".repeat(40)}]},
            {"role": "assistant", "content": "done"},
            {"role": "user", "content": "thanks"}
        ]})
    }

    fn forced(target: &'static str, a: Action) -> impl Fn(usize, &CanonMsg) -> Option<Action> {
        move |_, m| (m.source.as_deref() == Some("/r/a.rs") || m.source.as_deref() == Some(target)).then_some(a)
    }

    #[test]
    fn drop_removes_one_pair_of_parallel_calls_and_keeps_the_rest_valid() {
        let body = anthropic_body();
        assert_eq!(validate_request(Provider::Anthropic, &body), Ok(()));
        // Force-drop everything touching a.rs (its call and result).
        let (_, out, _) = pipeline(Provider::Anthropic, &body, 100_000, &|_, m| {
            (m.source.as_deref() == Some("/r/a.rs")).then_some(Action::Drop)
        });
        let out = out.unwrap();
        assert_eq!(validate_request(Provider::Anthropic, &out), Ok(()));
        let s = out.to_string();
        assert!(!s.contains("fn a()") && s.contains("fn b()"), "{s}");
        // b's pair is intact: one tool_use and one tool_result remain.
        assert_eq!(s.matches("\"tool_use\"").count(), 1);
        assert_eq!(s.matches("\"tool_result\"").count(), 1);
    }

    #[test]
    fn dropping_all_blocks_removes_the_whole_message() {
        let body = anthropic_body();
        let (_, out, _) =
            pipeline(Provider::Anthropic, &body, 100_000, &|_, m| m.tool_id.is_some().then_some(Action::Drop));
        let out = out.unwrap();
        assert_eq!(validate_request(Provider::Anthropic, &out), Ok(()));
        // The tool-result message vanished entirely; the assistant message kept its text block.
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[1]["content"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn summarize_replaces_tool_result_content_and_keeps_the_call_verbatim() {
        let body = anthropic_body();
        let (_, out, _) = pipeline(Provider::Anthropic, &body, 100_000, &forced("/r/b.rs", Action::Summarize));
        let out = out.unwrap();
        assert_eq!(validate_request(Provider::Anthropic, &out), Ok(()));
        let s = out.to_string();
        assert!(s.contains("[loom summary of 1 item]"));
        assert!(!s.contains("fn b()"));
        assert!(s.contains("\"/r/b.rs\""), "call arguments stay verbatim");
    }

    #[test]
    fn untouched_request_is_returned_unchanged() {
        let body = anthropic_body();
        let (p, out, _) = pipeline(Provider::Anthropic, &body, 100_000, &|_, _| None);
        assert!(p.items.iter().all(|i| i.decision == Action::Keep));
        assert_eq!(out.unwrap(), body);
    }

    #[test]
    fn unrewritable_blocks_are_rejected_if_a_plan_touches_them() {
        let body = json!({"messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [{"type": "thinking", "thinking": "hmm", "signature": "sig"}, {"type": "text", "text": "ok"}]},
            {"role": "user", "content": "more"}]});
        let n = normalize(Provider::Anthropic, body.to_string().as_bytes()).unwrap();
        assert!(n.msgs[1].opaque, "thinking blocks are locked");
        let (p, out, _) = pipeline(Provider::Anthropic, &body, 100_000, &|_, m| m.opaque.then_some(Action::Drop));
        assert_eq!(p.items[1].decision, Action::Keep, "the policy refuses first");
        assert_eq!(out.unwrap(), body);
    }

    #[test]
    fn openai_tool_calls_drop_and_summarize() {
        let body = json!({"model": "m", "messages": [
            {"role": "system", "content": "s"},
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": null, "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"/r/a.rs\"}"}},
                {"id": "c2", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"/r/b.rs\"}"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "fn a() {}
".repeat(40)},
            {"role": "tool", "tool_call_id": "c2", "content": "fn b() {}
".repeat(40)},
            {"role": "assistant", "content": "done"},
            {"role": "user", "content": "thanks"}]});
        assert_eq!(validate_request(Provider::OpenAi, &body), Ok(()));
        let (_, out, _) = pipeline(Provider::OpenAi, &body, 100_000, &|_, m| {
            (m.source.as_deref() == Some("/r/a.rs")
                || m.source.as_deref() == Some("read_file") && m.content.contains("a.rs"))
            .then_some(Action::Drop)
        });
        let out = out.unwrap();
        assert_eq!(validate_request(Provider::OpenAi, &out), Ok(()));
        let s = out.to_string();
        assert!(!s.contains("\"c1\"") && s.contains("\"c2\""), "{s}");

        // Dropping every call removes the empty assistant message too.
        let (_, out, _) =
            pipeline(Provider::OpenAi, &body, 100_000, &|_, m| m.tool_id.is_some().then_some(Action::Drop));
        let out = out.unwrap();
        assert_eq!(validate_request(Provider::OpenAi, &out), Ok(()));
        assert_eq!(out["messages"].as_array().unwrap().len(), 4); // system, user, "done", user

        // Summarizing results replaces tool message content only.
        let (_, out, _) = pipeline(Provider::OpenAi, &body, 100_000, &|_, m| {
            (m.kind == NodeKind::File || m.kind == NodeKind::ToolResult).then_some(Action::Summarize)
        });
        let out = out.unwrap();
        assert_eq!(validate_request(Provider::OpenAi, &out), Ok(()));
        assert!(out.to_string().contains("[loom summary of 1 item]"));
    }

    #[test]
    fn validation_catches_broken_pairs() {
        let orphan_result = json!({"messages": [
            {"role": "user", "content": "go"},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "x", "content": "r"}]}]});
        assert!(validate_request(Provider::Anthropic, &orphan_result).is_err());
        let unanswered = json!({"messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "x", "name": "T", "input": {}}]},
            {"role": "user", "content": "no result here"}]});
        assert!(validate_request(Provider::Anthropic, &unanswered).is_err());
        let starts_with_assistant = json!({"messages": [{"role": "assistant", "content": "hi"}]});
        assert!(validate_request(Provider::Anthropic, &starts_with_assistant).is_err());
        let oai_orphan = json!({"messages": [{"role": "user", "content": "go"}, {"role": "tool", "tool_call_id": "z", "content": "r"}]});
        assert!(validate_request(Provider::OpenAi, &oai_orphan).is_err());
    }

    const MEMORY: &str = "[loom notes] Facts the user wants you to keep in mind:\n- average(): divides by n+1";

    #[test]
    fn edited_text_replaces_what_the_model_sees_and_nothing_else_changes() {
        let body = anthropic_body();
        let (_, out, _, applied) = pipeline_with(
            Provider::Anthropic,
            &body,
            100_000,
            &|_, _| None,
            &|_, m| m.content.starts_with("fn a").then(|| "EDITED RESULT A".to_string()),
            None,
        );
        assert_eq!(applied.len(), 1);
        let out = out.unwrap();
        assert_eq!(validate_request(Provider::Anthropic, &out), Ok(()));
        let s = out.to_string();
        assert!(s.contains("EDITED RESULT A") && !s.contains("fn a()"));
        assert!(s.contains("fn b()"), "other results are untouched");
        // the tool pair is intact: the call is still there next to its (edited) result
        assert_eq!(s.matches("\"tool_use\"").count(), 2);
        assert_eq!(s.matches("\"tool_result\"").count(), 2);
        // and the original body was not modified in place
        assert!(body.to_string().contains("fn a()"));
    }

    #[test]
    fn edited_text_works_for_messages_and_openai_tool_output() {
        let body = anthropic_body();
        let (_, out, _, _) = pipeline_with(
            Provider::Anthropic,
            &body,
            100_000,
            &|_, _| None,
            &|_, m| (m.content == "thanks").then(|| "thanks a lot".to_string()),
            None,
        );
        assert!(out.unwrap().to_string().contains("thanks a lot"));

        let oai = json!({"model": "m", "messages": [
            {"role": "system", "content": "s"},
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"/r/a.rs\"}"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "original output"},
            {"role": "user", "content": "thanks"}]});
        let (_, out, _, _) = pipeline_with(
            Provider::OpenAi,
            &oai,
            100_000,
            &|_, _| None,
            &|_, m| (m.content == "original output").then(|| "edited output".to_string()),
            None,
        );
        let out = out.unwrap();
        assert_eq!(validate_request(Provider::OpenAi, &out), Ok(()));
        assert_eq!(out["messages"][3]["content"], "edited output");
        assert_eq!(out["messages"][3]["tool_call_id"], "c1");
    }

    #[test]
    fn an_edit_on_a_dropped_node_is_dropped_with_it() {
        let body = anthropic_body();
        let (_, out, _, _) = pipeline_with(
            Provider::Anthropic,
            &body,
            100_000,
            &|_, m| (m.source.as_deref() == Some("/r/a.rs")).then_some(Action::Drop),
            &|_, m| m.content.starts_with("fn a").then(|| "EDITED RESULT A".to_string()),
            None,
        );
        let s = out.unwrap().to_string();
        assert!(!s.contains("EDITED RESULT A") && !s.contains("fn a()"));
    }

    #[test]
    fn calls_and_the_system_prompt_are_never_overridden() {
        let body = anthropic_body();
        let n = normalize(Provider::Anthropic, body.to_string().as_bytes()).unwrap();
        let plan = Plan {
            budget: 1,
            items: n
                .msgs
                .iter()
                .enumerate()
                .map(|(i, _)| crate::policy::PlanItem {
                    id: format!("n{i}"),
                    decision: Action::Keep,
                    reason: String::new(),
                    score: 0.0,
                    tokens: 1,
                    planned_tokens: 1,
                    required: false,
                })
                .collect(),
            original_tokens: 0,
            planned_tokens: 0,
            must_tokens: 0,
            over_budget: false,
            cache_prefix_len: 0,
            prefix_frozen: false,
        };
        let call = n.msgs.iter().position(|m| m.kind == NodeKind::ToolCall).unwrap();
        let sys = n.msgs.iter().position(|m| m.kind == NodeKind::System).unwrap();
        for i in [call, sys] {
            let o: HashMap<String, String> = [(format!("n{i}"), "x".to_string())].into();
            let r = rewrite_with(
                Provider::Anthropic,
                &body,
                &n.msgs,
                &plan,
                &HashMap::new(),
                &Extras { overrides: Some(&o), memory: None },
            );
            assert!(r.is_err(), "node {i} must not be editable");
        }
    }

    #[test]
    fn notes_go_at_the_end_of_the_last_user_message() {
        // plain text content
        let body = json!({"model": "m", "messages": [{"role": "user", "content": "first"}, {"role": "assistant", "content": "ok"}, {"role": "user", "content": "last question"}]});
        let (_, out, _, _) =
            pipeline_with(Provider::Anthropic, &body, 100_000, &|_, _| None, &|_, _| None, Some(MEMORY));
        let out = out.unwrap();
        assert_eq!(validate_request(Provider::Anthropic, &out), Ok(()));
        assert_eq!(out["messages"][2]["content"], format!("last question\n\n{MEMORY}"));
        assert_eq!(
            out["messages"][0]["content"], "first",
            "earlier messages (and the provider cache for them) are untouched"
        );

        // a message of tool results: the notes become a text block after the results
        let with_results = json!({"model": "m", "messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "a", "name": "Read", "input": {"file_path": "/r/a.rs"}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": "body"}]}]});
        let (_, out, _, _) =
            pipeline_with(Provider::Anthropic, &with_results, 100_000, &|_, _| None, &|_, _| None, Some(MEMORY));
        let out = out.unwrap();
        assert_eq!(validate_request(Provider::Anthropic, &out), Ok(()));
        let blocks = out["messages"][2]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "tool_result", "tool results stay first");
        assert_eq!(blocks[1], json!({"type": "text", "text": MEMORY}));
    }

    #[test]
    fn notes_for_openai_requests_that_end_with_tool_output_become_a_new_user_message() {
        let oai = json!({"model": "m", "messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "output"}]});
        let (_, out, _, _) = pipeline_with(Provider::OpenAi, &oai, 100_000, &|_, _| None, &|_, _| None, Some(MEMORY));
        let out = out.unwrap();
        assert_eq!(validate_request(Provider::OpenAi, &out), Ok(()));
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[3], json!({"role": "user", "content": MEMORY}));
    }

    #[test]
    fn an_anthropic_prefill_is_left_alone_and_the_notes_use_the_last_user_message() {
        let mut msgs = vec![
            json!({"role": "user", "content": "question"}),
            json!({"role": "assistant", "content": "The answer is"}),
        ];
        inject_memory(Provider::Anthropic, &mut msgs, MEMORY);
        assert_eq!(msgs[1]["content"], "The answer is");
        assert_eq!(msgs[0]["content"], format!("question\n\n{MEMORY}"));
    }

    #[test]
    fn without_edits_or_notes_the_body_is_unchanged() {
        let body = anthropic_body();
        let (_, out, _, applied) = pipeline_with(Provider::Anthropic, &body, 100_000, &|_, _| None, &|_, _| None, None);
        assert!(applied.is_empty());
        assert_eq!(out.unwrap(), body);
        assert!(Extras::default().is_empty());
        assert!(!Extras { memory: Some("x"), ..Default::default() }.is_empty());
    }

    // ------------------------------------------------ end-to-end property tests

    #[derive(Debug, Clone)]
    enum Turn {
        Chat(String),
        Tools(Vec<(u8, usize)>), // (file, output length)
    }

    fn turn() -> impl Strategy<Value = Turn> {
        prop_oneof![
            "[a-z][a-z ]{0,59}".prop_map(Turn::Chat),
            prop::collection::vec((0u8..3, 1usize..800), 1..4).prop_map(Turn::Tools),
        ]
    }

    fn anthropic_conversation(turns: &[Turn], thinking_at: Option<usize>) -> Value {
        let mut msgs = vec![json!({"role": "user", "content": "start the task"})];
        for (ti, t) in turns.iter().enumerate() {
            match t {
                Turn::Chat(s) => {
                    msgs.push(json!({"role": "assistant", "content": s}));
                    msgs.push(json!({"role": "user", "content": format!("next {s}")}));
                }
                Turn::Tools(calls) => {
                    let mut uses = vec![];
                    let mut results = vec![];
                    if thinking_at == Some(ti) {
                        uses.push(json!({"type": "thinking", "thinking": "hm", "signature": "sig"}));
                    }
                    for (k, (file, len)) in calls.iter().enumerate() {
                        let id = format!("t{ti}_{k}");
                        uses.push(json!({"type": "tool_use", "id": id, "name": "Read", "input": {"file_path": format!("/r/f{file}.rs")}}));
                        results.push(json!({"type": "tool_result", "tool_use_id": id, "content": "x".repeat(*len)}));
                    }
                    msgs.push(json!({"role": "assistant", "content": uses}));
                    msgs.push(json!({"role": "user", "content": results}));
                }
            }
        }
        msgs.push(json!({"role": "user", "content": "final question"}));
        json!({"model": "m", "system": "sys", "messages": msgs})
    }

    fn openai_conversation(turns: &[Turn]) -> Value {
        let mut msgs =
            vec![json!({"role": "system", "content": "sys"}), json!({"role": "user", "content": "start the task"})];
        for (ti, t) in turns.iter().enumerate() {
            match t {
                Turn::Chat(s) => {
                    msgs.push(json!({"role": "assistant", "content": s}));
                    msgs.push(json!({"role": "user", "content": format!("next {s}")}));
                }
                Turn::Tools(calls) => {
                    let tcs: Vec<Value> = calls
                        .iter()
                        .enumerate()
                        .map(|(k, (file, _))| json!({"id": format!("c{ti}_{k}"), "type": "function", "function": {"name": "read_file", "arguments": format!("{{\"path\":\"/r/f{file}.rs\"}}")}}))
                        .collect();
                    msgs.push(json!({"role": "assistant", "content": null, "tool_calls": tcs}));
                    for (k, (_, len)) in calls.iter().enumerate() {
                        msgs.push(
                            json!({"role": "tool", "tool_call_id": format!("c{ti}_{k}"), "content": "x".repeat(*len)}),
                        );
                    }
                }
            }
        }
        msgs.push(json!({"role": "user", "content": "final question"}));
        json!({"model": "m", "messages": msgs})
    }

    /// Roughly one in four eligible nodes is edited.
    fn edits(seed: u64) -> impl Fn(usize, &CanonMsg) -> Option<String> {
        move |i, _| ((seed >> (i % 50)) & 3 == 0).then(|| format!("EDITED {i} for the model"))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(300))]

        #[test]
        fn edits_and_notes_never_break_a_request(
            turns in prop::collection::vec(turn(), 0..14),
            budget in 0u32..6000,
            thinking in proptest::option::of(0usize..14),
            seed in any::<u64>(),
            openai in any::<bool>(),
            notes in any::<bool>(),
        ) {
            let (provider, body) = if openai {
                (Provider::OpenAi, openai_conversation(&turns))
            } else {
                (Provider::Anthropic, anthropic_conversation(&turns, thinking))
            };
            let memory = notes.then_some(MEMORY);
            let force = move |i: usize, _: &CanonMsg| match (seed >> ((i + 7) % 60)) & 3 {
                0 => Some(Action::Drop),
                1 => Some(Action::Summarize),
                _ => None,
            };
            let (p, out, msgs, applied) = pipeline_with(provider, &body, budget, &force, &edits(seed), memory);
            let out = out.map_err(|e| TestCaseError::fail(format!("rewrite failed: {e}")))?;
            prop_assert_eq!(validate_request(provider, &out), Ok(()));
            let s = out.to_string();
            // Notes appear exactly once, and only when asked for.
            prop_assert_eq!(s.matches("[loom notes]").count(), usize::from(notes));
            // An edit shows up if and only if its node was kept.
            for (item, m) in p.items.iter().zip(&msgs) {
                if let Some(text) = applied.get(&item.id) {
                    let present = s.contains(text.as_str());
                    prop_assert_eq!(present, item.decision == Action::Keep, "{} ({:?}) visible={} decision={:?}", item.id, m.kind, present, item.decision);
                }
            }
        }

        #[test]
        fn rewritten_anthropic_requests_always_validate(
            turns in prop::collection::vec(turn(), 0..14),
            budget in 0u32..4000,
            thinking in proptest::option::of(0usize..14),
            seed in any::<u64>(),
        ) {
            let body = anthropic_conversation(&turns, thinking);
            prop_assert_eq!(validate_request(Provider::Anthropic, &body), Ok(()));
            // Random per-node forced actions on top of the budget pressure.
            let (_, out, _) = pipeline(Provider::Anthropic, &body, budget, &move |i, _| match (seed >> (i % 60)) & 3 {
                0 => Some(Action::Drop),
                1 => Some(Action::Summarize),
                _ => None,
            });
            let out = out.map_err(|e| TestCaseError::fail(format!("rewrite failed: {e}")))?;
            prop_assert_eq!(validate_request(Provider::Anthropic, &out), Ok(()));
        }

        #[test]
        fn rewritten_openai_requests_always_validate(
            turns in prop::collection::vec(turn(), 0..14),
            budget in 0u32..4000,
            seed in any::<u64>(),
        ) {
            let body = openai_conversation(&turns);
            prop_assert_eq!(validate_request(Provider::OpenAi, &body), Ok(()));
            let (_, out, _) = pipeline(Provider::OpenAi, &body, budget, &move |i, _| match (seed >> (i % 60)) & 3 {
                0 => Some(Action::Drop),
                1 => Some(Action::Summarize),
                _ => None,
            });
            let out = out.map_err(|e| TestCaseError::fail(format!("rewrite failed: {e}")))?;
            prop_assert_eq!(validate_request(Provider::OpenAi, &out), Ok(()));
        }
    }
}
