//! Convert Anthropic / OpenAI request bodies into one internal format.

use std::collections::HashMap;

use serde_json::Value;

use crate::model::{CanonMsg, Loc, NodeKind, Provider, Role};

pub struct Normalized {
    pub model: Option<String>,
    pub msgs: Vec<CanonMsg>,
    /// The parsed request body, so callers need not parse it again.
    pub raw: Value,
}

pub fn normalize(provider: Provider, body: &[u8]) -> Result<Normalized, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let model = v.get("model").and_then(Value::as_str).map(String::from);
    let mut msgs = match provider {
        Provider::Anthropic => anthropic(&v),
        Provider::OpenAi => openai(&v),
    }
    .ok_or("missing messages array")?;
    classify(&mut msgs);
    Ok(Normalized { model, msgs, raw: v })
}

fn msg(role: Role, kind: NodeKind, content: String, loc: Loc) -> CanonMsg {
    CanonMsg { role, kind, content, source: None, tool_id: None, loc, opaque: false }
}

fn at(msg: usize, block: Option<usize>) -> Loc {
    Loc::Msg { msg, block }
}

/// Plain text of a content value; non-text blocks become canonical JSON or a marker.
fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Array(a) => a
            .iter()
            .map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b.get("text").and_then(Value::as_str).unwrap_or("").trim().to_string(),
                Some("image") | Some("image_url") => image_marker(b),
                _ => b.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Images carry huge base64 payloads; keep only a short identity marker.
fn image_marker(b: &Value) -> String {
    format!("[image {}]", &blake3::hash(b.to_string().as_bytes()).to_hex()[..16])
}

fn anthropic(v: &Value) -> Option<Vec<CanonMsg>> {
    let messages = v.get("messages")?.as_array()?;
    let mut out = Vec::new();
    if let Some(sys) = v.get("system") {
        let t = text_of(sys);
        if !t.is_empty() {
            out.push(msg(Role::System, NodeKind::System, t, Loc::System));
        }
    }
    for (mi, m) in messages.iter().enumerate() {
        let (role, plain) = match m.get("role").and_then(Value::as_str) {
            Some("assistant") => (Role::Assistant, NodeKind::Assistant),
            _ => (Role::User, NodeKind::User),
        };
        match m.get("content") {
            Some(Value::String(s)) => {
                if !s.trim().is_empty() {
                    out.push(msg(role, plain, s.trim().to_string(), at(mi, None)));
                }
            }
            Some(Value::Array(blocks)) => {
                for (bi, b) in blocks.iter().enumerate() {
                    let loc = at(mi, Some(bi));
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let t = b.get("text").and_then(Value::as_str).unwrap_or("").trim();
                            if !t.is_empty() {
                                out.push(msg(role, plain, t.to_string(), loc));
                            }
                        }
                        Some("tool_use") => {
                            let mut c = msg(
                                role,
                                NodeKind::ToolCall,
                                b.get("input").map(Value::to_string).unwrap_or_default(),
                                loc,
                            );
                            c.source = b.get("name").and_then(Value::as_str).map(String::from);
                            c.tool_id = b.get("id").and_then(Value::as_str).map(String::from);
                            out.push(c);
                        }
                        Some("tool_result") => {
                            let mut c =
                                msg(role, NodeKind::ToolResult, b.get("content").map(text_of).unwrap_or_default(), loc);
                            c.tool_id = b.get("tool_use_id").and_then(Value::as_str).map(String::from);
                            // A tool result carrying images/documents cannot be replaced by text safely.
                            c.opaque = b.get("content").and_then(Value::as_array).is_some_and(|a| {
                                a.iter().any(|p| p.get("type").and_then(Value::as_str) != Some("text"))
                            });
                            out.push(c);
                        }
                        Some("image") => {
                            let mut c = msg(role, plain, image_marker(b), loc);
                            c.opaque = true;
                            out.push(c);
                        }
                        _ => {
                            // thinking, redacted_thinking, documents, ...: must be sent back untouched.
                            let mut c = msg(role, plain, b.to_string(), loc);
                            c.opaque = true;
                            out.push(c);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Some(out)
}

fn openai(v: &Value) -> Option<Vec<CanonMsg>> {
    let messages = v.get("messages")?.as_array()?;
    let mut out = Vec::new();
    for (mi, m) in messages.iter().enumerate() {
        let content = m.get("content").map(text_of).unwrap_or_default();
        // Parts other than plain text (images, audio) cannot be replaced by a text summary.
        let rich = m
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|p| p.get("type").and_then(Value::as_str) != Some("text")));
        let loc = at(mi, None);
        match m.get("role").and_then(Value::as_str) {
            Some("system") | Some("developer") => {
                if !content.is_empty() {
                    out.push(msg(Role::System, NodeKind::System, content, loc));
                }
            }
            Some("assistant") => {
                if !content.is_empty() {
                    let mut c = msg(Role::Assistant, NodeKind::Assistant, content, loc);
                    c.opaque = rich;
                    out.push(c);
                }
                for (k, tc) in m.get("tool_calls").and_then(Value::as_array).into_iter().flatten().enumerate() {
                    let f = tc.get("function");
                    let args = f.and_then(|f| f.get("arguments")).and_then(Value::as_str).unwrap_or("");
                    let canon = serde_json::from_str::<Value>(args)
                        .map(|v| v.to_string())
                        .unwrap_or_else(|_| args.trim().to_string());
                    let mut c = msg(Role::Assistant, NodeKind::ToolCall, canon, at(mi, Some(k)));
                    c.source = f.and_then(|f| f.get("name")).and_then(Value::as_str).map(String::from);
                    c.tool_id = tc.get("id").and_then(Value::as_str).map(String::from);
                    out.push(c);
                }
            }
            Some("tool") => {
                let mut c = msg(Role::Tool, NodeKind::ToolResult, content, loc);
                c.tool_id = m.get("tool_call_id").and_then(Value::as_str).map(String::from);
                c.opaque = rich;
                out.push(c);
            }
            _ => {
                if !content.is_empty() {
                    let mut c = msg(Role::User, NodeKind::User, content, loc);
                    c.opaque = rich;
                    out.push(c);
                }
            }
        }
    }
    Some(out)
}

const READ_TOOLS: &[&str] = &["read", "read_file", "readfile", "view", "view_file", "cat", "open_file"];

/// Refine ToolResult nodes using the tool call they answer: File for read
/// tools (source = path), Memory for memory tools (source = key), otherwise
/// source = tool name.
fn classify(msgs: &mut [CanonMsg]) {
    let calls: HashMap<String, (String, String)> = msgs
        .iter()
        .filter(|m| m.kind == NodeKind::ToolCall)
        .filter_map(|m| Some((m.tool_id.clone()?, (m.source.clone()?, m.content.clone()))))
        .collect();
    for m in msgs.iter_mut().filter(|m| m.kind == NodeKind::ToolResult) {
        let Some((name, input)) = m.tool_id.as_ref().and_then(|id| calls.get(id)) else {
            continue;
        };
        let lname = name.to_lowercase();
        if lname.contains("memory") {
            m.kind = NodeKind::Memory;
            m.source = Some(field(input, &["key", "path", "name"]).unwrap_or_else(|| name.clone()));
        } else if READ_TOOLS.contains(&lname.as_str()) {
            m.kind = NodeKind::File;
            m.source = Some(field(input, &["file_path", "path", "filename", "file"]).unwrap_or_else(|| name.clone()));
        } else {
            m.source = Some(name.clone());
        }
    }
}

fn field(json: &str, keys: &[&str]) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str)).map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_blocks_split_and_classify() {
        let body = br#"{"model":"m","system":[{"type":"text","text":" sys "}],"messages":[
            {"role":"user","content":"task"},
            {"role":"assistant","content":[{"type":"text","text":"reading"},
              {"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"src/a.rs"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"fn a(){}"}]}]}
        ]}"#;
        let n = normalize(Provider::Anthropic, body).unwrap();
        assert_eq!(n.model.as_deref(), Some("m"));
        let kinds: Vec<_> = n.msgs.iter().map(|m| m.kind).collect();
        assert_eq!(kinds, [NodeKind::System, NodeKind::User, NodeKind::Assistant, NodeKind::ToolCall, NodeKind::File]);
        assert_eq!(n.msgs[0].content, "sys");
        assert_eq!(n.msgs[4].source.as_deref(), Some("src/a.rs"));
        assert_eq!(n.msgs[4].content, "fn a(){}");
    }

    #[test]
    fn openai_tool_calls_and_memory() {
        let body = br#"{"messages":[
            {"role":"system","content":"s"},
            {"role":"user","content":"u"},
            {"role":"assistant","content":null,"tool_calls":[{"id":"c1","function":{"name":"memory_get","arguments":"{ \"key\": \"k1\" }"}}]},
            {"role":"tool","tool_call_id":"c1","content":"v"}
        ]}"#;
        let n = normalize(Provider::OpenAi, body).unwrap();
        assert_eq!(n.msgs.len(), 4);
        assert_eq!(n.msgs[2].kind, NodeKind::ToolCall);
        assert_eq!(n.msgs[2].content, r#"{"key":"k1"}"#);
        assert_eq!(n.msgs[3].kind, NodeKind::Memory);
        assert_eq!(n.msgs[3].source.as_deref(), Some("k1"));
    }

    #[test]
    fn bad_bodies_error() {
        assert!(normalize(Provider::Anthropic, b"nope").is_err());
        assert!(normalize(Provider::OpenAi, b"{}").is_err());
    }
}
