//! Policy engine: decides keep / summarize / drop for every node of the next
//! request. Pure and deterministic, so it is unit- and property-testable.
//!
//! Precedence: hard constraints > user action > rule > algorithm.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::graph::Edge;
use crate::model::{Action, EdgeKind, NodeKind, Provider};

// ---------------------------------------------------------------- inputs

/// Scoring weights and thresholds. Missing fields fall back to defaults, so
/// partial JSON (a per-session override) works.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Weights {
    pub w_imp: f32,
    pub w_rec: f32,
    pub w_ref: f32,
    pub w_task: f32,
    pub w_sup: f32,
    /// Recency decay constant, in turns.
    pub tau: f32,
    /// Minimum score for a node to be worth summarizing instead of dropping.
    pub summarize_threshold: f32,
    /// How many trailing nodes are always kept (the newest exchange).
    pub protect_tail: usize,
}

impl Default for Weights {
    fn default() -> Self {
        Self { w_imp: 1.0, w_rec: 1.0, w_ref: 0.5, w_task: 1.0, w_sup: 1.5, tau: 8.0, summarize_threshold: 0.8, protect_tail: 4 }
    }
}

/// Conditions are ANDed. An empty matcher matches nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Matcher {
    pub kind: Option<NodeKind>,
    /// `*` and `?` wildcards, matched against the node source (path or tool name).
    pub source_glob: Option<String>,
    /// Matches nodes with more tokens than this.
    pub min_tokens: Option<u32>,
    /// Matches nodes first seen more than this many turns ago.
    pub older_than_turns: Option<u32>,
}

impl Matcher {
    pub fn is_empty(&self) -> bool {
        self.kind.is_none() && self.source_glob.is_none() && self.min_tokens.is_none() && self.older_than_turns.is_none()
    }

    pub fn matches(&self, n: &PNode, age_turns: u32) -> bool {
        !self.is_empty()
            && self.kind.is_none_or(|k| k == n.kind)
            && self.min_tokens.is_none_or(|t| n.tokens > t)
            && self.older_than_turns.is_none_or(|t| age_turns > t)
            && self
                .source_glob
                .as_deref()
                .is_none_or(|g| n.source.as_deref().is_some_and(|s| glob_match(g, s)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub id: String,
    pub session_id: Option<String>,
    pub matcher: Matcher,
    pub action: Action,
    pub priority: i32,
}

/// A node as the policy sees it. `PolicyInput::nodes` is in request order.
#[derive(Debug, Clone)]
pub struct PNode {
    pub id: String,
    pub kind: NodeKind,
    pub content: String,
    pub tokens: u32,
    pub first_seen_turn: u32,
    pub source: Option<String>,
    pub importance: f32,
    pub pinned: bool,
    pub user_action: Option<Action>,
}

#[derive(Debug, Clone)]
pub struct PolicyInput {
    pub nodes: Vec<PNode>,
    pub edges: Vec<Edge>,
    pub latest_turn: u32,
    pub budget: u32,
    pub weights: Weights,
    pub rules: Vec<PolicyRule>,
    pub provider: Provider,
}

// ---------------------------------------------------------------- outputs

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanItem {
    pub id: String,
    pub decision: Action,
    pub reason: String,
    pub score: f32,
    pub tokens: u32,
    /// Tokens this node costs under the decision (0 dropped, estimate if summarized).
    pub planned_tokens: u32,
    /// True when a hard constraint forces the node to stay.
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plan {
    pub budget: u32,
    pub items: Vec<PlanItem>,
    pub original_tokens: u32,
    pub planned_tokens: u32,
    /// Tokens of required nodes plus forced summaries: the floor the plan cannot go below.
    pub must_tokens: u32,
    /// The floor alone exceeds the budget; everything else is dropped.
    pub over_budget: bool,
    pub cache_prefix_len: usize,
    /// Unchanged prefix was held fixed to protect prompt caching.
    pub prefix_frozen: bool,
}

// ---------------------------------------------------------------- helpers

/// Summaries do not exist until Phase 6; this estimates their size.
pub fn summary_estimate(tokens: u32) -> u32 {
    (tokens / 5).clamp(24, 400).min(tokens)
}

/// Default budget: model window minus reserved output tokens.
pub fn budget_for(model: Option<&str>) -> u32 {
    let m = model.unwrap_or("").to_lowercase();
    if m.contains("claude") {
        200_000 - 8_192
    } else if m.contains("gpt-4o") || m.contains("gpt-4.1") || m.contains("gpt-5") {
        128_000 - 16_384
    } else {
        128_000 - 8_192
    }
}

pub fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

fn words(text: &str) -> HashSet<String> {
    text.chars()
        .take(4000)
        .collect::<String>()
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(String::from)
        .collect()
}

/// Leading nodes that existed before this turn: the prefix a provider can
/// serve from its prompt cache if we leave it untouched.
pub fn cache_prefix_len(nodes: &[PNode], latest_turn: u32) -> usize {
    nodes.iter().take_while(|n| n.first_seen_turn < latest_turn).count()
}

// ---------------------------------------------------------------- scoring

/// score = w_imp*importance + w_rec*exp(-age/tau) + w_ref*ref_in_degree
///       + w_task*relevance - w_sup*superseded            (index-aligned with `nodes`)
pub fn score_nodes(inp: &PolicyInput) -> Vec<f32> {
    let w = &inp.weights;
    let mut refs: HashMap<&str, u32> = HashMap::new();
    let mut superseded: HashSet<&str> = HashSet::new();
    for e in &inp.edges {
        match e.kind {
            EdgeKind::RefersTo => *refs.entry(e.to.as_str()).or_default() += 1,
            EdgeKind::Supersedes => {
                superseded.insert(e.to.as_str());
            }
            _ => {}
        }
    }
    let query = inp.nodes.iter().rev().find(|n| n.kind == NodeKind::User).map(|n| words(&n.content)).unwrap_or_default();
    inp.nodes
        .iter()
        .map(|n| {
            let age = inp.latest_turn.saturating_sub(n.first_seen_turn) as f32;
            let recency = (-age / w.tau.max(0.01)).exp();
            let ref_deg = (*refs.get(n.id.as_str()).unwrap_or(&0) as f32 / 3.0).min(1.0);
            let relevance = if query.is_empty() {
                0.0
            } else {
                (words(&n.content).intersection(&query).count() as f32 / query.len() as f32).min(1.0)
            };
            let sup = if superseded.contains(n.id.as_str()) { 1.0 } else { 0.0 };
            let s = w.w_imp * n.importance.clamp(0.0, 1.0) + w.w_rec * recency + w.w_ref * ref_deg + w.w_task * relevance - w.w_sup * sup;
            if s.is_finite() {
                s
            } else {
                0.0
            }
        })
        .collect()
}

/// Per-node forced action: the user's choice wins, else the highest-priority matching rule.
pub fn apply_rules(nodes: &[PNode], latest_turn: u32, rules: &[PolicyRule]) -> Vec<Option<(Action, String)>> {
    nodes
        .iter()
        .map(|n| {
            if let Some(a) = n.user_action {
                return Some((a, "set by user".to_string()));
            }
            let age = latest_turn.saturating_sub(n.first_seen_turn);
            rules
                .iter()
                .filter(|r| r.matcher.matches(n, age))
                .max_by_key(|r| r.priority) // ties keep the earliest rule
                .map(|r| (r.action, format!("rule {} (priority {})", short(&r.id), r.priority)))
        })
        .collect()
}

fn short(id: &str) -> &str {
    id.get(id.len().saturating_sub(6)..).unwrap_or(id)
}

fn safety(a: Action) -> u8 {
    match a {
        Action::Keep => 2,
        Action::Summarize => 1,
        Action::Drop => 0,
    }
}

// ---------------------------------------------------------------- planning

struct Group {
    members: Vec<usize>,
    tokens: u32,
    est: u32,
    score: f32,
    required: Option<String>,
    forced: Option<(Action, String)>,
    in_prefix: bool,
}

/// Decide keep / summarize / drop for each node within the token budget.
pub fn plan(inp: &PolicyInput) -> Plan {
    let nodes = &inp.nodes;
    let n = nodes.len();
    let w = &inp.weights;
    let scores = score_nodes(inp);
    let forced = apply_rules(nodes, inp.latest_turn, &inp.rules);
    let prefix_len = cache_prefix_len(nodes, inp.latest_turn);
    let idx: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, x)| (x.id.as_str(), i)).collect();

    // Tool pairs form groups that always share one decision.
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for e in inp.edges.iter().filter(|e| e.kind == EdgeKind::ToolPair) {
        if let (Some(&a), Some(&b)) = (idx.get(e.from.as_str()), idx.get(e.to.as_str())) {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            if ra != rb {
                parent[rb.max(ra)] = rb.min(ra);
            }
        }
    }
    let mut group_of = vec![0usize; n];
    let mut members: Vec<Vec<usize>> = Vec::new();
    let mut root_group: HashMap<usize, usize> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        let g = *root_group.entry(r).or_insert_with(|| {
            members.push(Vec::new());
            members.len() - 1
        });
        members[g].push(i);
        group_of[i] = g;
    }

    // Hard constraints (node level).
    let last_user = nodes.iter().rposition(|x| x.kind == NodeKind::User);
    let first_user = nodes.iter().position(|x| x.kind == NodeKind::User);
    let tail_start = n.saturating_sub(w.protect_tail);
    let required_reason = |i: usize| -> Option<String> {
        let x = &nodes[i];
        if x.kind == NodeKind::System {
            Some("system prompt".into())
        } else if x.pinned {
            Some("pinned".into())
        } else if Some(i) == last_user {
            Some("latest user message".into())
        } else if inp.provider == Provider::Anthropic && Some(i) == first_user {
            Some("first message must be a user message".into())
        } else if i >= tail_start {
            Some("recent context".into())
        } else if let Some((Action::Keep, why)) = &forced[i] {
            Some(format!("kept: {why}"))
        } else {
            None
        }
    };

    let mut groups: Vec<Group> = members
        .into_iter()
        .map(|ms| {
            let tokens: u32 = ms.iter().map(|&i| nodes[i].tokens).sum();
            let est: u32 = ms.iter().map(|&i| summary_estimate(nodes[i].tokens)).sum();
            let score = ms.iter().map(|&i| scores[i]).fold(f32::MIN, f32::max);
            let mut required = ms.iter().find_map(|&i| required_reason(i));
            if let (Some(r), true) = (&mut required, ms.len() > 1) {
                *r = format!("{r} (tool pair stays together)");
            }
            // Safest forced action among members wins, so a pair never splits.
            let forced = ms.iter().filter_map(|&i| forced[i].clone()).max_by_key(|(a, _)| safety(*a));
            let in_prefix = ms.iter().any(|&i| i < prefix_len);
            Group { members: ms, tokens, est, score, required, forced, in_prefix }
        })
        .collect();

    let budget = inp.budget as u64;
    let mut decision: Vec<Option<(Action, String)>> = vec![None; groups.len()];
    let mut used: u64 = 0;

    // 1. Required groups and forced summaries.
    for (g, grp) in groups.iter_mut().enumerate() {
        if let Some(why) = &grp.required {
            let note = match &grp.forced {
                Some((Action::Drop | Action::Summarize, _)) => " (user/rule request to shrink it ignored)",
                _ => "",
            };
            decision[g] = Some((Action::Keep, format!("Kept: {why}{note}")));
            used += grp.tokens as u64;
        } else if let Some((a, why)) = &grp.forced {
            match a {
                Action::Keep => {} // already handled as required
                Action::Summarize => {
                    decision[g] = Some((Action::Summarize, format!("Summarized: {why}")));
                    used += grp.est as u64;
                }
                Action::Drop => decision[g] = Some((Action::Drop, format!("Dropped: {why}"))),
            }
        }
    }
    let must_tokens = used;
    let over_budget = must_tokens > budget;

    // 2. Hold the unchanged prefix fixed when it still fits (protects prompt caching).
    let frozen: Vec<usize> =
        (0..groups.len()).filter(|&g| decision[g].is_none() && groups[g].in_prefix).collect();
    let frozen_tokens: u64 = frozen.iter().map(|&g| groups[g].tokens as u64).sum();
    let prefix_frozen = prefix_len > 0 && !over_budget && !frozen.is_empty() && used + frozen_tokens <= budget;
    if prefix_frozen {
        for &g in &frozen {
            decision[g] = Some((Action::Keep, "Kept: unchanged prefix (protects prompt cache)".into()));
        }
        used += frozen_tokens;
    }

    // 3. Greedy knapsack on score density for everything left.
    let mut rest: Vec<usize> = (0..groups.len()).filter(|&g| decision[g].is_none()).collect();
    rest.sort_by(|&a, &b| {
        let (da, db) = (groups[a].score / groups[a].tokens.max(1) as f32, groups[b].score / groups[b].tokens.max(1) as f32);
        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal).then(groups[a].members[0].cmp(&groups[b].members[0]))
    });
    let superseded_by: HashMap<&str, &str> = inp
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Supersedes)
        .map(|e| (e.to.as_str(), e.from.as_str()))
        .collect();
    for g in rest {
        let grp = &groups[g];
        let tokens = grp.tokens as u64;
        let sup = grp
            .members
            .iter()
            .find_map(|&i| superseded_by.get(nodes[i].id.as_str()))
            .map(|by| format!(", superseded by {}", short(by)))
            .unwrap_or_default();
        decision[g] = Some(if over_budget {
            (Action::Drop, "Dropped: required items already exceed the budget".into())
        } else if used + tokens <= budget {
            used += tokens;
            (Action::Keep, format!("Kept: fits budget (score {:.2})", grp.score))
        } else if used + grp.est as u64 <= budget && grp.score > w.summarize_threshold {
            used += grp.est as u64;
            (Action::Summarize, format!("Summarized: no room in full; score {:.2} above {:.2}", grp.score, w.summarize_threshold))
        } else if grp.score > w.summarize_threshold {
            (Action::Drop, format!("Dropped: no room even summarized (score {:.2}{sup})", grp.score))
        } else {
            (Action::Drop, format!("Dropped: score {:.2} below {:.2} and no room{sup}", grp.score, w.summarize_threshold))
        });
    }

    // Assemble in request order.
    let items: Vec<PlanItem> = (0..n)
        .map(|i| {
            let g = group_of[i];
            let (action, reason) = decision[g].clone().expect("every group decided");
            let planned = match action {
                Action::Keep => nodes[i].tokens,
                Action::Summarize => summary_estimate(nodes[i].tokens),
                Action::Drop => 0,
            };
            let reason = if groups[g].members.len() > 1 && !reason.contains("tool pair") {
                format!("{reason} · tool pair")
            } else {
                reason
            };
            PlanItem {
                id: nodes[i].id.clone(),
                decision: action,
                reason,
                score: scores[i],
                tokens: nodes[i].tokens,
                planned_tokens: planned,
                required: groups[g].required.is_some(),
            }
        })
        .collect();

    Plan {
        budget: inp.budget,
        original_tokens: nodes.iter().map(|x| x.tokens).sum(),
        planned_tokens: items.iter().map(|x| x.planned_tokens).sum(),
        must_tokens: must_tokens.min(u32::MAX as u64) as u32,
        over_budget,
        cache_prefix_len: prefix_len,
        prefix_frozen,
        items,
    }
}

/// Why a node got its decision (for the UI).
pub fn explain<'a>(plan: &'a Plan, id: &str) -> Option<&'a PlanItem> {
    plan.items.iter().find(|i| i.id == id)
}

/// Invariants every plan must satisfy; the rewriter reuses this before sending.
pub fn validate_plan(inp: &PolicyInput, plan: &Plan) -> Result<(), String> {
    let nodes = &inp.nodes;
    if plan.items.len() != nodes.len() || plan.items.iter().zip(nodes).any(|(i, n)| i.id != n.id) {
        return Err("items do not match nodes".into());
    }
    let dec: HashMap<&str, Action> = plan.items.iter().map(|i| (i.id.as_str(), i.decision)).collect();
    for e in inp.edges.iter().filter(|e| e.kind == EdgeKind::ToolPair) {
        if let (Some(a), Some(b)) = (dec.get(e.from.as_str()), dec.get(e.to.as_str())) {
            if a != b {
                return Err(format!("tool pair split: {} is {a:?}, {} is {b:?}", e.from, e.to));
            }
        }
    }
    let last_user = nodes.iter().rposition(|x| x.kind == NodeKind::User);
    let first_user = nodes.iter().position(|x| x.kind == NodeKind::User);
    for (i, (n, item)) in nodes.iter().zip(&plan.items).enumerate() {
        let must = n.kind == NodeKind::System
            || n.pinned
            || Some(i) == last_user
            || (inp.provider == Provider::Anthropic && Some(i) == first_user);
        if must && item.decision != Action::Keep {
            return Err(format!("required node {} was {:?}", n.id, item.decision));
        }
        let expect = match item.decision {
            Action::Keep => n.tokens,
            Action::Summarize => summary_estimate(n.tokens),
            Action::Drop => 0,
        };
        if item.planned_tokens != expect {
            return Err(format!("token accounting wrong for {}", n.id));
        }
    }
    if plan.planned_tokens != plan.items.iter().map(|i| i.planned_tokens).sum::<u32>() {
        return Err("planned_tokens does not match items".into());
    }
    if plan.over_budget != (plan.must_tokens > plan.budget) {
        return Err("over_budget flag inconsistent".into());
    }
    if !plan.over_budget && plan.planned_tokens > plan.budget {
        return Err(format!("plan {} exceeds budget {} although the required set fits", plan.planned_tokens, plan.budget));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{build_edges, GNode};
    use proptest::prelude::*;

    fn node(id: &str, kind: NodeKind, tokens: u32, turn: u32, content: &str) -> PNode {
        PNode { id: id.into(), kind, content: content.into(), tokens, first_seen_turn: turn, source: None, importance: 0.5, pinned: false, user_action: None }
    }

    fn input(nodes: Vec<PNode>, edges: Vec<Edge>, budget: u32, latest: u32) -> PolicyInput {
        PolicyInput { nodes, edges, latest_turn: latest, budget, weights: Weights::default(), rules: vec![], provider: Provider::Anthropic }
    }

    fn decisions(p: &Plan) -> Vec<(&str, Action)> {
        p.items.iter().map(|i| (i.id.as_str(), i.decision)).collect()
    }

    #[test]
    fn everything_is_kept_when_it_fits() {
        let nodes = vec![node("s", NodeKind::System, 10, 0, "sys"), node("u", NodeKind::User, 10, 0, "hi"), node("a", NodeKind::Assistant, 10, 1, "yo")];
        let p = plan(&input(nodes.clone(), vec![], 1000, 1));
        assert!(p.items.iter().all(|i| i.decision == Action::Keep));
        assert!(validate_plan(&input(nodes, vec![], 1000, 1), &p).is_ok());
    }

    #[test]
    fn low_value_old_bulk_is_dropped_under_pressure() {
        let mut nodes = vec![node("s", NodeKind::System, 10, 0, "sys"), node("u0", NodeKind::User, 10, 0, "fix the parser")];
        nodes.push(node("log", NodeKind::ToolResult, 5000, 1, "irrelevant noise"));
        nodes.push(node("good", NodeKind::ToolResult, 100, 20, "parser fix details"));
        for i in 0..4 {
            nodes.push(node(&format!("t{i}"), NodeKind::Assistant, 10, 20, "ok"));
        }
        nodes.push(node("u1", NodeKind::User, 10, 20, "fix the parser now"));
        let inp = input(nodes, vec![], 600, 20);
        let p = plan(&inp);
        assert!(validate_plan(&inp, &p).is_ok());
        assert_eq!(explain(&p, "log").unwrap().decision, Action::Drop);
        assert_eq!(explain(&p, "good").unwrap().decision, Action::Keep);
    }

    #[test]
    fn pinned_node_survives_even_when_user_says_drop() {
        let mut nodes = vec![node("s", NodeKind::System, 10, 0, "sys"), node("u", NodeKind::User, 10, 0, "hi")];
        let mut p1 = node("p", NodeKind::ToolResult, 100, 0, "important");
        p1.pinned = true;
        p1.user_action = Some(Action::Drop);
        nodes.push(p1);
        for i in 0..5 {
            nodes.push(node(&format!("x{i}"), NodeKind::Assistant, 50, 9, "filler"));
        }
        let inp = input(nodes, vec![], 150, 9);
        let p = plan(&inp);
        let item = explain(&p, "p").unwrap();
        assert_eq!(item.decision, Action::Keep);
        assert!(item.reason.contains("pinned") && item.reason.contains("ignored"), "{}", item.reason);
    }

    #[test]
    fn user_action_beats_rule_and_rule_beats_algorithm() {
        let mut a = node("a", NodeKind::ToolResult, 50, 0, "x");
        a.user_action = Some(Action::Drop);
        let b = node("b", NodeKind::ToolResult, 50, 0, "y");
        let mut nodes = vec![node("s", NodeKind::System, 5, 0, "s"), node("u", NodeKind::User, 5, 0, "u"), a, b];
        for i in 0..4 {
            nodes.push(node(&format!("t{i}"), NodeKind::Assistant, 5, 10, "t"));
        }
        let mut inp = input(nodes, vec![], 10_000, 10);
        inp.rules = vec![PolicyRule {
            id: "rule-abc123".into(),
            session_id: None,
            matcher: Matcher { kind: Some(NodeKind::ToolResult), older_than_turns: Some(5), ..Default::default() },
            action: Action::Summarize,
            priority: 1,
        }];
        let p = plan(&inp);
        let d = decisions(&p);
        assert!(d.contains(&("a", Action::Drop)), "user action wins: {d:?}");
        assert!(d.contains(&("b", Action::Summarize)), "rule applies without budget pressure: {d:?}");
        assert!(explain(&p, "b").unwrap().reason.contains("rule"));
    }

    #[test]
    fn higher_priority_rule_wins() {
        let n = node("n", NodeKind::ToolResult, 50, 0, "x");
        let m = Matcher { kind: Some(NodeKind::ToolResult), ..Default::default() };
        let rules = vec![
            PolicyRule { id: "r1".into(), session_id: None, matcher: m.clone(), action: Action::Drop, priority: 1 },
            PolicyRule { id: "r2".into(), session_id: None, matcher: m, action: Action::Keep, priority: 5 },
        ];
        assert_eq!(apply_rules(&[n], 3, &rules)[0].as_ref().unwrap().0, Action::Keep);
    }

    #[test]
    fn tool_pair_moves_as_one_even_if_only_one_side_is_forced() {
        let mut call = node("call", NodeKind::ToolCall, 20, 0, "{}");
        call.user_action = Some(Action::Drop);
        let res = node("res", NodeKind::ToolResult, 200, 0, "out");
        let mut nodes = vec![node("s", NodeKind::System, 5, 0, "s"), node("u", NodeKind::User, 5, 0, "u"), call, res];
        for i in 0..4 {
            nodes.push(node(&format!("t{i}"), NodeKind::Assistant, 5, 10, "t"));
        }
        let edges = vec![Edge { from: "call".into(), to: "res".into(), kind: EdgeKind::ToolPair, weight: 1.0 }];
        let inp = input(nodes, edges, 10_000, 10);
        let p = plan(&inp);
        assert_eq!(explain(&p, "call").unwrap().decision, Action::Drop);
        assert_eq!(explain(&p, "res").unwrap().decision, Action::Drop);
        assert!(validate_plan(&inp, &p).is_ok());
    }

    #[test]
    fn required_floor_over_budget_drops_the_rest_and_flags_it() {
        let mut nodes = vec![node("s", NodeKind::System, 500, 0, "s"), node("u", NodeKind::User, 500, 0, "u")];
        nodes.push(node("old", NodeKind::Assistant, 10, 0, "old"));
        for i in 0..4 {
            nodes.push(node(&format!("t{i}"), NodeKind::Assistant, 100, 5, "t"));
        }
        let inp = input(nodes, vec![], 600, 5);
        let p = plan(&inp);
        assert!(p.over_budget);
        assert_eq!(explain(&p, "old").unwrap().decision, Action::Drop);
        assert!(validate_plan(&inp, &p).is_ok());
    }

    #[test]
    fn unchanged_prefix_is_frozen_when_it_fits_and_released_when_not() {
        let mut nodes = vec![node("s", NodeKind::System, 10, 0, "s"), node("u", NodeKind::User, 10, 0, "do it")];
        nodes.push(node("junk", NodeKind::ToolResult, 100, 1, "zzz"));
        for i in 0..4 {
            nodes.push(node(&format!("t{i}"), NodeKind::Assistant, 10, 5, "t"));
        }
        let fits = plan(&input(nodes.clone(), vec![], 1000, 5));
        assert!(fits.prefix_frozen);
        assert!(fits.cache_prefix_len >= 3);

        let tight = plan(&input(nodes, vec![], 80, 5));
        assert!(!tight.prefix_frozen || tight.planned_tokens <= 80);
    }

    #[test]
    fn superseded_read_scores_below_fresh_read() {
        let mut old = node("old", NodeKind::File, 100, 1, "fn a");
        old.source = Some("a.rs".into());
        let mut new = node("new", NodeKind::File, 100, 1, "fn a");
        new.source = Some("a.rs".into());
        let edges = vec![Edge { from: "new".into(), to: "old".into(), kind: EdgeKind::Supersedes, weight: 1.0 }];
        let s = score_nodes(&input(vec![old, new], edges, 1000, 2));
        assert!(s[0] < s[1]);
    }

    #[test]
    fn glob_and_matcher() {
        assert!(glob_match("*.log", "/var/x/build.log"));
        assert!(glob_match("src/*/a?.rs", "src/core/ab.rs"));
        assert!(!glob_match("*.log", "build.txt"));
        assert!(!Matcher::default().matches(&node("n", NodeKind::User, 1, 0, ""), 0), "empty matcher matches nothing");
    }

    #[test]
    fn budget_for_models() {
        assert_eq!(budget_for(Some("claude-sonnet-5-5")), 191_808);
        assert_eq!(budget_for(None), 119_808);
    }

    // ------------------------------------------------ property tests

    #[derive(Debug, Clone)]
    enum Step {
        User(u32),
        Text(u32),
        Tool { file: u8, size: u32 },
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            (1u32..300).prop_map(Step::User),
            (1u32..300).prop_map(Step::Text),
            (0u8..3, 1u32..3000).prop_map(|(file, size)| Step::Tool { file, size }),
        ]
    }

    fn rule() -> impl Strategy<Value = PolicyRule> {
        (
            proptest::option::of(prop::sample::select(vec![NodeKind::ToolResult, NodeKind::File, NodeKind::Assistant, NodeKind::ToolCall])),
            proptest::option::of(0u32..2000),
            proptest::option::of(0u32..10),
            prop::sample::select(vec![Action::Keep, Action::Summarize, Action::Drop]),
            -3i32..3,
        )
            .prop_map(|(kind, min_tokens, older, action, priority)| PolicyRule {
                id: "r".into(),
                session_id: None,
                matcher: Matcher { kind, source_glob: None, min_tokens, older_than_turns: older },
                action,
                priority,
            })
    }

    /// Build a realistic request: system, user, then random steps, with real graph edges.
    fn build(steps: &[Step], flags: &[(bool, u8, u8)], budget: u32, provider: Provider, rules: Vec<PolicyRule>, tail: usize) -> PolicyInput {
        let mut nodes = vec![node("n0", NodeKind::System, 40, 0, "system rules"), node("n1", NodeKind::User, 20, 0, "refactor the parser module")];
        let mut meta: Vec<(Option<String>, Option<String>)> = vec![(None, None), (None, None)]; // source, tool_id
        let mut turn = 0u32;
        let push = |nodes: &mut Vec<PNode>, meta: &mut Vec<(Option<String>, Option<String>)>, mut n: PNode, source: Option<String>, tid: Option<String>| {
            n.id = format!("n{}", nodes.len());
            n.source = source.clone();
            nodes.push(n);
            meta.push((source, tid));
        };
        for (i, s) in steps.iter().enumerate() {
            turn += (i % 2) as u32;
            match s {
                Step::User(t) => push(&mut nodes, &mut meta, node("", NodeKind::User, *t, turn, "parser module please"), None, None),
                Step::Text(t) => push(&mut nodes, &mut meta, node("", NodeKind::Assistant, *t, turn, "working on the parser"), None, None),
                Step::Tool { file, size } => {
                    let path = format!("/repo/f{file}.rs");
                    let tid = format!("t{i}");
                    push(&mut nodes, &mut meta, node("", NodeKind::ToolCall, 15, turn, &format!("{{\"file_path\":\"{path}\"}}")), Some("Read".into()), Some(tid.clone()));
                    push(&mut nodes, &mut meta, node("", NodeKind::File, *size, turn, "file body"), Some(path), Some(tid));
                }
            }
        }
        for (i, n) in nodes.iter_mut().enumerate() {
            let (pin, imp, act) = flags[i % flags.len()];
            n.pinned = pin && i % 7 == 0;
            n.importance = imp as f32 / 255.0;
            n.user_action = match act % 8 {
                1 => Some(Action::Keep),
                2 => Some(Action::Summarize),
                3 => Some(Action::Drop),
                _ => None,
            };
        }
        let g: Vec<GNode> = nodes
            .iter()
            .zip(&meta)
            .map(|(n, (src, tid))| GNode { id: &n.id, kind: n.kind, source: src.as_deref(), tool_id: tid.as_deref(), text: Some(&n.content) })
            .collect();
        let all: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        let edges = build_edges(&g, &all);
        PolicyInput { nodes, edges, latest_turn: turn, budget, weights: Weights { protect_tail: tail, ..Weights::default() }, rules, provider }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]

        #[test]
        fn every_plan_is_valid(
            steps in prop::collection::vec(step(), 0..40),
            flags in prop::collection::vec((any::<bool>(), any::<u8>(), any::<u8>()), 1..12),
            budget in 0u32..20_000,
            openai in any::<bool>(),
            rules in prop::collection::vec(rule(), 0..3),
            tail in 0usize..8,
        ) {
            let provider = if openai { Provider::OpenAi } else { Provider::Anthropic };
            let inp = build(&steps, &flags, budget, provider, rules, tail);
            let p = plan(&inp);
            prop_assert_eq!(validate_plan(&inp, &p), Ok(()));
            // Deterministic: same input, same plan.
            prop_assert_eq!(&plan(&inp), &p);
        }

        #[test]
        fn generous_budget_without_rules_or_overrides_keeps_everything(
            steps in prop::collection::vec(step(), 0..30),
        ) {
            let mut inp = build(&steps, &[(false, 128, 0)], u32::MAX / 2, Provider::Anthropic, vec![], 4);
            inp.nodes.iter_mut().for_each(|n| n.user_action = None);
            let p = plan(&inp);
            prop_assert!(p.items.iter().all(|i| i.decision == Action::Keep));
            prop_assert_eq!(p.planned_tokens, p.original_tokens);
        }
    }
}
