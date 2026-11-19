//! Relations between context nodes. Pure functions over one request's ordered
//! node list (oldest first), so no database reads are needed to build edges.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::model::{EdgeKind, NodeKind};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub weight: f32,
}

/// A node as seen in the current request.
#[derive(Debug, Clone, Copy)]
pub struct GNode<'a> {
    pub id: &'a str,
    pub kind: NodeKind,
    pub source: Option<&'a str>,
    pub tool_id: Option<&'a str>,
    pub text: Option<&'a str>,
}

fn edge(from: &str, to: &str, kind: EdgeKind) -> Edge {
    Edge { from: from.into(), to: to.into(), kind, weight: 1.0 }
}

fn is_result(k: NodeKind) -> bool {
    matches!(k, NodeKind::ToolResult | NodeKind::File | NodeKind::Memory)
}

/// tool call -> its result, matched by tool id. A hard constraint for the policy engine.
pub fn link_tool_pairs(nodes: &[GNode]) -> Vec<Edge> {
    let calls: HashMap<&str, &str> =
        nodes.iter().filter(|n| n.kind == NodeKind::ToolCall).filter_map(|n| Some((n.tool_id?, n.id))).collect();
    nodes
        .iter()
        .filter(|n| is_result(n.kind))
        .filter_map(|n| Some(edge(calls.get(n.tool_id?)?, n.id, EdgeKind::ToolPair)))
        .collect()
}

/// Each node follows the one before it (later -> earlier).
pub fn link_follows(nodes: &[GNode]) -> Vec<Edge> {
    nodes.windows(2).map(|w| edge(w[1].id, w[0].id, EdgeKind::Follows)).collect()
}

/// A newer read of the same file (or value of the same memory key) supersedes
/// the previous one (newer -> older). Chains link neighbours only.
pub fn link_supersedes(nodes: &[GNode]) -> Vec<Edge> {
    let mut last: HashMap<&str, &str> = HashMap::new();
    let mut out = Vec::new();
    for n in nodes.iter().filter(|n| matches!(n.kind, NodeKind::File | NodeKind::Memory)) {
        let Some(src) = n.source else { continue };
        if let Some(prev) = last.insert(src, n.id) {
            out.push(edge(n.id, prev, EdgeKind::Supersedes));
        }
    }
    out
}

/// Path-like tokens in free text: `src/a.rs`, `a.rs:12`, `C:\repo\a.rs`.
/// Deliberately loose; matching against real node sources removes false positives.
pub fn extract_refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let is_sep = |c: char| c.is_whitespace() || "\"'`()[]{},;<>=*|".contains(c);
    for tok in text.split(is_sep) {
        let tok = tok.trim_end_matches(['.', ':', '!', '?']);
        let tok = strip_line_suffix(tok);
        if tok.len() < 3 || tok.contains("://") {
            continue;
        }
        let norm = normalize_path(tok);
        let has_ext = norm.rsplit_once('.').is_some_and(|(name, ext)| {
            !name.is_empty()
                && (1..=6).contains(&ext.len())
                && ext.chars().any(|c| c.is_alphabetic())
                && ext.chars().all(|c| c.is_alphanumeric())
        });
        if (norm.contains('/') || has_ext) && !out.contains(&norm) {
            out.push(norm);
        }
    }
    out
}

/// `a.rs:12` and `a.rs:12:3` -> `a.rs`.
fn strip_line_suffix(tok: &str) -> &str {
    let mut t = tok;
    for _ in 0..2 {
        match t.rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) && !head.is_empty() => {
                t = head
            }
            _ => break,
        }
    }
    t
}

/// Forward slashes only; runs of backslashes (JSON-escaped Windows paths) collapse to one.
fn normalize_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut prev_slash = false;
    for c in p.chars() {
        if c == '\\' {
            if !prev_slash {
                out.push('/');
            }
            prev_slash = true;
        } else {
            out.push(c);
            prev_slash = false;
        }
    }
    out
}

/// Does a mention (`auth.rs`, `src/auth.rs`) point at this source path?
fn mentions(source: &str, mention: &str) -> bool {
    let s = normalize_path(source);
    s == mention || s.ends_with(&format!("/{mention}")) || mention.ends_with(&format!("/{s}"))
}

/// Assistant text or tool input that names a file/memory source seen earlier
/// links to the latest earlier node with that source. Only nodes in `eval` are
/// scanned (the new ones), which keeps per-request cost proportional to new text.
pub fn link_refers_to(nodes: &[GNode], eval: &HashSet<&str>) -> Vec<Edge> {
    let mut latest: HashMap<&str, &str> = HashMap::new(); // source -> latest earlier node id
    let mut out = Vec::new();
    for n in nodes {
        if matches!(n.kind, NodeKind::Assistant | NodeKind::ToolCall) && eval.contains(n.id) {
            if let Some(text) = n.text {
                let refs = extract_refs(text);
                if !refs.is_empty() {
                    for (src, id) in &latest {
                        if refs.iter().any(|r| mentions(src, r)) {
                            out.push(edge(n.id, id, EdgeKind::RefersTo));
                        }
                    }
                }
            }
        }
        if matches!(n.kind, NodeKind::File | NodeKind::Memory) {
            if let Some(src) = n.source {
                latest.insert(src, n.id);
            }
        }
    }
    out.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    out
}

/// All edges that touch at least one node in `new`.
pub fn build_edges(nodes: &[GNode], new: &HashSet<&str>) -> Vec<Edge> {
    let mut all = link_tool_pairs(nodes);
    all.extend(link_follows(nodes));
    all.extend(link_supersedes(nodes));
    all.extend(link_refers_to(nodes, new));
    all.retain(|e| new.contains(e.from.as_str()) || new.contains(e.to.as_str()));
    all
}

/// Nodes that would lose meaning if `id` were dropped: its tool-call partner and
/// anything that refers to it or was derived from it.
pub fn dependents(id: &str, edges: &[Edge]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in edges {
        let other = match e.kind {
            EdgeKind::ToolPair if e.from == id => &e.to,
            EdgeKind::ToolPair if e.to == id => &e.from,
            EdgeKind::RefersTo | EdgeKind::DerivedFrom if e.to == id => &e.from,
            _ => continue,
        };
        if !out.contains(other) {
            out.push(other.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use NodeKind::*;

    fn n<'a>(
        id: &'a str,
        kind: NodeKind,
        source: Option<&'a str>,
        tool_id: Option<&'a str>,
        text: Option<&'a str>,
    ) -> GNode<'a> {
        GNode { id, kind, source, tool_id, text }
    }

    /// system, user, call(Read a.rs), file a.rs, assistant mentions a.rs, call(Read a.rs), file a.rs (again)
    fn transcript() -> Vec<GNode<'static>> {
        vec![
            n("sys", System, None, None, Some("rules")),
            n("u", User, None, None, Some("fix auth")),
            n("c1", ToolCall, Some("Read"), Some("t1"), Some(r#"{"file_path":"/repo/src/a.rs"}"#)),
            n("f1", File, Some("/repo/src/a.rs"), Some("t1"), Some("fn a(){}")),
            n("as", Assistant, None, None, Some("The bug is in src/a.rs:12, not b.")),
            n("c2", ToolCall, Some("Read"), Some("t2"), Some(r#"{"file_path":"/repo/src/a.rs"}"#)),
            n("f2", File, Some("/repo/src/a.rs"), Some("t2"), Some("fn a(){ fixed }")),
        ]
    }

    fn kinds(edges: &[Edge], k: EdgeKind) -> Vec<(&str, &str)> {
        edges.iter().filter(|e| e.kind == k).map(|e| (e.from.as_str(), e.to.as_str())).collect()
    }

    #[test]
    fn tool_pairs() {
        let e = link_tool_pairs(&transcript());
        assert_eq!(kinds(&e, EdgeKind::ToolPair), [("c1", "f1"), ("c2", "f2")]);
    }

    #[test]
    fn tool_pair_needs_matching_id() {
        let t = vec![n("c", ToolCall, Some("X"), Some("a"), None), n("r", ToolResult, Some("X"), Some("b"), None)];
        assert!(link_tool_pairs(&t).is_empty());
    }

    #[test]
    fn follows_is_later_to_earlier() {
        let e = link_follows(&transcript());
        assert_eq!(e.len(), 6);
        assert_eq!((e[0].from.as_str(), e[0].to.as_str()), ("u", "sys"));
    }

    #[test]
    fn supersedes_newer_read_of_same_file() {
        let e = link_supersedes(&transcript());
        assert_eq!(kinds(&e, EdgeKind::Supersedes), [("f2", "f1")]);
    }

    #[test]
    fn supersedes_chains_neighbours_and_ignores_other_files() {
        let t = vec![
            n("a1", File, Some("a"), None, None),
            n("b1", File, Some("b"), None, None),
            n("a2", File, Some("a"), None, None),
            n("a3", File, Some("a"), None, None),
        ];
        assert_eq!(kinds(&link_supersedes(&t), EdgeKind::Supersedes), [("a2", "a1"), ("a3", "a2")]);
    }

    #[test]
    fn refers_to_resolves_relative_mention_to_absolute_source() {
        let t = transcript();
        let all: HashSet<&str> = t.iter().map(|x| x.id).collect();
        let e = link_refers_to(&t, &all);
        // The assistant's relative mention resolves to f1; call c2's input names the same path.
        assert_eq!(kinds(&e, EdgeKind::RefersTo), [("as", "f1"), ("c2", "f1")]);
    }

    #[test]
    fn refers_to_ignores_unrelated_text_and_later_nodes() {
        let t = vec![
            n("as", Assistant, None, None, Some("look at src/a.rs")),
            n("f", File, Some("/repo/src/a.rs"), None, None),
        ];
        let all: HashSet<&str> = t.iter().map(|x| x.id).collect();
        assert!(link_refers_to(&t, &all).is_empty(), "node cannot refer forward");
        let t2 = vec![
            n("f", File, Some("/repo/src/a.rs"), None, None),
            n("as", Assistant, None, None, Some("nothing about it, see b.rs")),
        ];
        let all2: HashSet<&str> = t2.iter().map(|x| x.id).collect();
        assert!(link_refers_to(&t2, &all2).is_empty());
    }

    #[test]
    fn refers_to_handles_windows_json_paths() {
        let t = vec![
            n("f", File, Some(r"C:\repo\src\a.rs"), None, None),
            n("c", ToolCall, Some("Edit"), Some("x"), Some(r#"{"file_path":"C:\\repo\\src\\a.rs"}"#)),
        ];
        let all: HashSet<&str> = t.iter().map(|x| x.id).collect();
        assert_eq!(kinds(&link_refers_to(&t, &all), EdgeKind::RefersTo), [("c", "f")]);
    }

    #[test]
    fn extract_refs_finds_paths_and_strips_line_numbers() {
        let r = extract_refs("see src/a.rs:12:3, (lib.rs) and https://x.io/p.html but not 1.2.3 or e.g.");
        assert!(r.contains(&"src/a.rs".to_string()));
        assert!(r.contains(&"lib.rs".to_string()));
        assert!(!r.iter().any(|x| x.contains("x.io")));
        assert!(!r.contains(&"1.2.3".to_string()));
    }

    #[test]
    fn build_edges_only_returns_edges_touching_new_nodes() {
        let t = transcript();
        let new: HashSet<&str> = ["c2", "f2"].into();
        let e = build_edges(&t, &new);
        assert!(e.iter().all(|x| new.contains(x.from.as_str()) || new.contains(x.to.as_str())));
        assert!(kinds(&e, EdgeKind::ToolPair).contains(&("c2", "f2")));
        assert!(!kinds(&e, EdgeKind::ToolPair).contains(&("c1", "f1")));
        assert!(kinds(&e, EdgeKind::Supersedes).contains(&("f2", "f1")));
    }

    #[test]
    fn dependents_include_partner_and_referrers() {
        let t = transcript();
        let all: HashSet<&str> = t.iter().map(|x| x.id).collect();
        let e = build_edges(&t, &all);
        let mut d = dependents("f1", &e);
        d.sort();
        assert_eq!(d, ["as", "c1", "c2"]);
        assert_eq!(dependents("u", &e), Vec::<String>::new());
    }
}
