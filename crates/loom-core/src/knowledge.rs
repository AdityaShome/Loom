//! A small knowledge graph of what an agent has seen and done: entities (files, symbols, commands,
//! errors, goals) and facts about them. This module extracts it from conversation nodes with fixed
//! rules, so it is pure and deterministic; a model can add more facts later. Everything here can be
//! edited by the user, which is why every fact carries a stable `key`.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::graph::extract_refs;
use crate::model::{EntityKind, NodeKind};

/// A node as the extractor needs it.
#[derive(Debug, Clone, Copy)]
pub struct KNode<'a> {
    pub id: &'a str,
    pub kind: NodeKind,
    pub source: Option<&'a str>,
    pub tool_id: Option<&'a str>,
    pub content: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityRef {
    pub kind: EntityKind,
    pub name: String,
}

impl EntityRef {
    pub fn new(kind: EntityKind, name: impl Into<String>) -> Self {
        Self { kind, name: name.into() }
    }
}

/// A fact the extractor derived. `subject --predicate--> object`, with optional free text.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoFact {
    pub subject: EntityRef,
    pub predicate: String,
    pub object: Option<EntityRef>,
    pub text: String,
    /// The node this came from.
    pub node_id: String,
}

impl AutoFact {
    /// Stable identity: the same fact extracted again has the same key, so re-extraction never duplicates it
    /// and a deletion can be remembered.
    pub fn key(&self) -> String {
        fact_key(&self.subject, &self.predicate, self.object.as_ref(), &self.text)
    }
}

pub fn fact_key(subject: &EntityRef, predicate: &str, object: Option<&EntityRef>, text: &str) -> String {
    let mut h = blake3::Hasher::new();
    let obj = object.map(|o| format!("{}:{}", o.kind.as_str(), o.name)).unwrap_or_default();
    for part in [subject.kind.as_str(), &subject.name, predicate, &obj, text] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().to_hex().to_string()
}

fn shorten(text: &str, max: usize) -> String {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= max {
        t
    } else {
        let cut: String = t.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

const EDIT_TOOLS: &[&str] = &[
    "edit",
    "edit_file",
    "write",
    "write_file",
    "str_replace",
    "str_replace_based_edit_tool",
    "multiedit",
    "apply_patch",
    "replace",
];
const SHELL_TOOLS: &[&str] = &["bash", "shell", "run_command", "exec", "execute", "cmd", "terminal", "run"];
const ERROR_WORDS: &[&str] = &["error", "failed", "panicked", "exception", "traceback", "assertion"];
const SYMBOL_PREFIXES: &[&str] = &[
    "pub async fn ",
    "pub fn ",
    "async fn ",
    "fn ",
    "pub struct ",
    "struct ",
    "pub enum ",
    "enum ",
    "pub trait ",
    "trait ",
    "def ",
    "async def ",
    "class ",
    "function ",
    "async function ",
    "export function ",
    "export async function ",
    "export class ",
    "interface ",
    "export interface ",
];

fn arg<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| args.get(*k).and_then(Value::as_str))
}

/// Names defined in a source file: functions, classes, structs and so on. First `max` only.
pub fn symbols(content: &str, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in content.lines() {
        let l = line.trim_start();
        let Some(rest) = SYMBOL_PREFIXES.iter().find_map(|p| l.strip_prefix(p)) else { continue };
        let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if name.len() >= 2 && !out.contains(&name) {
            out.push(name);
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// Lines that look like errors, trimmed. At most `max`.
pub fn error_lines(content: &str, max: usize) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| l.len() >= 8 && ERROR_WORDS.iter().any(|w| l.to_lowercase().contains(w)))
        .map(|l| shorten(l, 110))
        .take(max)
        .collect()
}

/// Facts for the nodes in `only`. `nodes` is the whole request in order: later nodes can refer to earlier tool
/// calls, so the call table is built from all of them.
pub fn extract(nodes: &[KNode], only: &HashSet<&str>) -> Vec<AutoFact> {
    let mut calls: HashMap<&str, (String, Value)> = HashMap::new();
    for n in nodes.iter().filter(|n| n.kind == NodeKind::ToolCall) {
        if let Some(tid) = n.tool_id {
            calls.insert(
                tid,
                (n.source.unwrap_or("tool").to_lowercase(), serde_json::from_str(n.content).unwrap_or(Value::Null)),
            );
        }
    }
    let first_user = nodes.iter().find(|n| n.kind == NodeKind::User).map(|n| n.id);

    let mut out: Vec<AutoFact> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |f: AutoFact| {
        if seen.insert(f.key()) {
            out.push(f);
        }
    };

    for n in nodes.iter().filter(|n| only.contains(n.id)) {
        match n.kind {
            NodeKind::User if Some(n.id) == first_user && !n.content.trim().is_empty() => push(AutoFact {
                subject: EntityRef::new(EntityKind::Goal, shorten(n.content, 80)),
                predicate: "task".into(),
                object: None,
                text: shorten(n.content, 240),
                node_id: n.id.into(),
            }),
            NodeKind::ToolCall => {
                let name = n.source.unwrap_or("tool").to_lowercase();
                let args: Value = serde_json::from_str(n.content).unwrap_or(Value::Null);
                if EDIT_TOOLS.contains(&name.as_str()) {
                    if let Some(path) = arg(&args, &["file_path", "path", "filename", "file"]) {
                        let text = match (
                            arg(&args, &["old", "old_string", "old_str"]),
                            arg(&args, &["new", "new_string", "new_str"]),
                        ) {
                            (Some(old), Some(new)) => {
                                format!("Edited: `{}` → `{}`", shorten(old, 70), shorten(new, 70))
                            }
                            _ if arg(&args, &["content"]).is_some() => "Wrote the file".to_string(),
                            _ => "Edited".to_string(),
                        };
                        push(AutoFact {
                            subject: EntityRef::new(EntityKind::File, path),
                            predicate: "edited".into(),
                            object: None,
                            text,
                            node_id: n.id.into(),
                        });
                    }
                }
            }
            NodeKind::ToolResult | NodeKind::File | NodeKind::Memory => {
                let call = n.tool_id.and_then(|t| calls.get(t));
                let errors = error_lines(n.content, 2);

                if n.kind == NodeKind::File {
                    if let Some(path) = n.source {
                        let file = EntityRef::new(EntityKind::File, path);
                        for s in symbols(n.content, 10) {
                            push(AutoFact {
                                subject: file.clone(),
                                predicate: "defines".into(),
                                object: Some(EntityRef::new(EntityKind::Symbol, s)),
                                text: String::new(),
                                node_id: n.id.into(),
                            });
                        }
                    }
                }

                if let Some((_, args)) = call.filter(|(t, _)| SHELL_TOOLS.contains(&t.as_str())) {
                    if let Some(cmd) = arg(args, &["command", "cmd"]) {
                        let command = EntityRef::new(EntityKind::Command, shorten(cmd, 80));
                        let first = n.content.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("(no output)");
                        push(AutoFact {
                            subject: command.clone(),
                            predicate: "result".into(),
                            object: None,
                            text: shorten(first, 160),
                            node_id: n.id.into(),
                        });
                        for e in &errors {
                            push(AutoFact {
                                subject: command.clone(),
                                predicate: "failed_with".into(),
                                object: Some(EntityRef::new(EntityKind::Error, e.clone())),
                                text: String::new(),
                                node_id: n.id.into(),
                            });
                        }
                    }
                } else {
                    // An error in a file or tool result: link it to the file it mentions, or the file it came from.
                    for e in &errors {
                        let file = extract_refs(e)
                            .into_iter()
                            .next()
                            .or_else(|| (n.kind == NodeKind::File).then(|| n.source.map(String::from)).flatten());
                        push(AutoFact {
                            subject: EntityRef::new(EntityKind::Error, e.clone()),
                            predicate: "reported_in".into(),
                            object: file.map(|f| EntityRef::new(EntityKind::File, f)),
                            text: String::new(),
                            node_id: n.id.into(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// A fact the user chose to send to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct InjectFact {
    pub subject: String,
    pub predicate: String,
    pub object: Option<String>,
    pub text: String,
}

/// How many characters of notes one request may carry.
pub const MEMORY_MAX_CHARS: usize = 4000;

/// The block appended to a request. `None` when there is nothing to say.
pub fn render_memory(facts: &[InjectFact]) -> Option<String> {
    if facts.is_empty() {
        return None;
    }
    let mut out = String::from("[loom notes] Facts the user wants you to keep in mind:");
    let mut used = 0;
    for f in facts {
        let line = match (&f.object, f.text.trim()) {
            (Some(o), "") => format!("{} {} {}", f.subject, f.predicate, o),
            (Some(o), t) => format!("{} {} {}: {}", f.subject, f.predicate, o, t),
            (None, "") => format!("{} ({})", f.subject, f.predicate),
            (None, t) => format!("{}: {}", f.subject, t),
        };
        if used + line.len() > MEMORY_MAX_CHARS {
            out.push_str("\n- ...(more notes omitted: too long)");
            break;
        }
        used += line.len();
        out.push_str("\n- ");
        out.push_str(&line);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k<'a>(
        id: &'a str,
        kind: NodeKind,
        source: Option<&'a str>,
        tool_id: Option<&'a str>,
        content: &'a str,
    ) -> KNode<'a> {
        KNode { id, kind, source, tool_id, content }
    }

    /// A short agent session: failed command, reads, an edit, a passing run.
    fn session() -> Vec<KNode<'static>> {
        vec![
            k("sys", NodeKind::System, None, None, "You are a coding assistant"),
            k("u", NodeKind::User, None, None, "Run the tests, fix the failing one, run them again"),
            k("c1", NodeKind::ToolCall, Some("run_command"), Some("t1"), r#"{"command":"cargo test"}"#),
            k("r1", NodeKind::ToolResult, Some("run_command"), Some("t1"), "exit code 101\nerror: could not find `Cargo.toml` in `/x` or any parent directory"),
            k("c2", NodeKind::ToolCall, Some("read_file"), Some("t2"), r#"{"path":"src/math.js"}"#),
            k("r2", NodeKind::File, Some("src/math.js"), Some("t2"), "function add(a, b) {\n  return a + b;\n}\n\nfunction average(numbers) {\n  return 1;\n}\n\nclass Stats {}\n"),
            k("c3", NodeKind::ToolCall, Some("edit_file"), Some("t3"), r#"{"path":"src/math.js","old":"return total / (numbers.length + 1);","new":"return total / numbers.length;"}"#),
            k("r3", NodeKind::ToolResult, Some("edit_file"), Some("t3"), "edited src/math.js"),
            k("c4", NodeKind::ToolCall, Some("run_command"), Some("t4"), r#"{"command":"npm test"}"#),
            k("r4", NodeKind::ToolResult, Some("run_command"), Some("t4"), "exit code 0\n✔ average is the mean"),
        ]
    }

    fn all<'a>(nodes: &[KNode<'a>]) -> HashSet<&'a str> {
        nodes.iter().map(|n| n.id).collect()
    }

    fn find<'a>(facts: &'a [AutoFact], subject: &str, predicate: &str) -> Vec<&'a AutoFact> {
        facts.iter().filter(|f| f.subject.name == subject && f.predicate == predicate).collect()
    }

    #[test]
    fn extracts_the_goal_from_the_first_user_message() {
        let nodes = session();
        let f = extract(&nodes, &all(&nodes));
        let goal = f.iter().find(|f| f.subject.kind == EntityKind::Goal).unwrap();
        assert_eq!(goal.predicate, "task");
        assert!(goal.text.starts_with("Run the tests, fix"));
        assert_eq!(goal.node_id, "u");
    }

    #[test]
    fn file_reads_yield_defined_symbols() {
        let nodes = session();
        let f = extract(&nodes, &all(&nodes));
        let defined: Vec<_> =
            find(&f, "src/math.js", "defines").iter().map(|x| x.object.as_ref().unwrap().name.clone()).collect();
        assert_eq!(defined, ["add", "average", "Stats"]);
        assert!(find(&f, "src/math.js", "defines")
            .iter()
            .all(|x| x.object.as_ref().unwrap().kind == EntityKind::Symbol && x.node_id == "r2"));
    }

    #[test]
    fn edits_record_what_changed() {
        let nodes = session();
        let f = extract(&nodes, &all(&nodes));
        let e = find(&f, "src/math.js", "edited");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].text, "Edited: `return total / (numbers.length + 1);` → `return total / numbers.length;`");
        assert_eq!(e[0].subject.kind, EntityKind::File);
    }

    #[test]
    fn commands_record_their_result_and_failures() {
        let nodes = session();
        let f = extract(&nodes, &all(&nodes));
        let r = find(&f, "cargo test", "result");
        assert_eq!(r[0].text, "exit code 101");
        let failed = find(&f, "cargo test", "failed_with");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].object.as_ref().unwrap().kind, EntityKind::Error);
        assert!(failed[0].object.as_ref().unwrap().name.contains("could not find"));
        assert_eq!(find(&f, "npm test", "result")[0].text, "exit code 0");
        assert!(find(&f, "npm test", "failed_with").is_empty(), "a passing run has no error");
    }

    #[test]
    fn errors_in_file_output_link_to_the_mentioned_file() {
        let nodes = vec![
            k("c", NodeKind::ToolCall, Some("search"), Some("t"), r#"{"text":"x"}"#),
            k(
                "r",
                NodeKind::ToolResult,
                Some("search"),
                Some("t"),
                "src/a.rs:3: error[E0425]: cannot find value `x` in this scope",
            ),
        ];
        let f = extract(&nodes, &all(&nodes));
        let e = f.iter().find(|f| f.predicate == "reported_in").unwrap();
        assert_eq!(e.subject.kind, EntityKind::Error);
        assert_eq!(e.object.as_ref().unwrap().name, "src/a.rs");
    }

    #[test]
    fn only_the_requested_nodes_produce_facts_but_old_calls_still_explain_new_results() {
        let nodes = session();
        let only: HashSet<&str> = ["r4"].into();
        let f = extract(&nodes, &only);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].subject.name, "npm test");
        assert_eq!(f[0].text, "exit code 0");
    }

    #[test]
    fn extraction_is_deterministic_and_keys_are_stable() {
        let nodes = session();
        let a = extract(&nodes, &all(&nodes));
        let b = extract(&nodes, &all(&nodes));
        assert_eq!(a, b);
        let keys: HashSet<String> = a.iter().map(AutoFact::key).collect();
        assert_eq!(keys.len(), a.len(), "no duplicate facts");
        let moved = AutoFact { node_id: "other".into(), ..a[0].clone() };
        assert_eq!(moved.key(), a[0].key(), "the source node is not part of identity");
        let changed = AutoFact { text: "different".into(), ..a[0].clone() };
        assert_ne!(changed.key(), a[0].key());
    }

    #[test]
    fn symbols_cover_common_languages_and_cap() {
        let src = "pub async fn serve() {}\ndef helper(x):\n  pass\nexport class Widget {}\nstruct Point;\nfn a() {}\nfn b() {}\n";
        // one-letter names are skipped as noise
        assert_eq!(symbols(src, 10), ["serve", "helper", "Widget", "Point"]);
        assert_eq!(symbols(src, 2).len(), 2);
        assert!(symbols("let function_call = 1;", 5).is_empty(), "only declarations count");
    }

    #[test]
    fn memory_renders_facts_the_user_chose() {
        assert_eq!(render_memory(&[]), None);
        let facts = vec![
            InjectFact {
                subject: "average()".into(),
                predicate: "has_bug".into(),
                object: None,
                text: "divides by n+1".into(),
            },
            InjectFact {
                subject: "math.test.js".into(),
                predicate: "tests".into(),
                object: Some("math.js".into()),
                text: String::new(),
            },
        ];
        let m = render_memory(&facts).unwrap();
        assert!(m.starts_with("[loom notes]"));
        assert!(m.contains("\n- average(): divides by n+1"));
        assert!(m.contains("\n- math.test.js tests math.js"));
    }

    #[test]
    fn memory_is_capped() {
        let facts: Vec<InjectFact> = (0..200)
            .map(|i| InjectFact {
                subject: format!("thing {i}"),
                predicate: "is".into(),
                object: None,
                text: "x".repeat(100),
            })
            .collect();
        let m = render_memory(&facts).unwrap();
        assert!(m.len() < MEMORY_MAX_CHARS + 300);
        assert!(m.ends_with("(more notes omitted: too long)"));
    }
}
