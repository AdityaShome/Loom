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
        Self {
            w_imp: 1.0,
            w_rec: 1.0,
            w_ref: 0.5,
            w_task: 1.0,
            w_sup: 1.5,
            tau: 8.0,
            summarize_threshold: 0.8,
            protect_tail: 4,
        }
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
        self.kind.is_none()
            && self.source_glob.is_none()
            && self.min_tokens.is_none()
            && self.older_than_turns.is_none()
    }

    pub fn matches(&self, n: &PNode, age_turns: u32) -> bool {
        !self.is_empty()
            && self.kind.is_none_or(|k| k == n.kind)
            && self.min_tokens.is_none_or(|t| n.tokens > t)
            && self.older_than_turns.is_none_or(|t| age_turns > t)
            && self.source_glob.as_deref().is_none_or(|g| n.source.as_deref().is_some_and(|s| glob_match(g, s)))
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
    pub content_hash: [u8; 32],
    pub tokens: u32,
    pub first_seen_turn: u32,
    pub source: Option<String>,
    pub importance: f32,
    pub pinned: bool,
    /// Block type that cannot be rewritten (thinking, images, ...): always kept.
    pub locked: bool,
    pub user_action: Option<Action>,
    /// Real size of the cached summary of this node, when one exists.
    pub summary_tokens: Option<u32>,
    /// What was actually sent for this node last call (`None` = it is new).
    pub prev: Option<Action>,
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
    /// Nodes whose summary is unavailable (not ready in time, or failed): they
    /// can only be kept or dropped.
    pub no_summary: HashSet<String>,
}

// ---------------------------------------------------------------- outputs

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanItem {
    pub id: String,
    pub decision: Action,
    pub reason: String,
    pub score: f32,
    pub tokens: u32,
    /// Tokens this node costs under the decision (0 dropped, summary size if summarized).
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
    /// The previous call's decisions for the unchanged prefix were repeated to protect prompt caching.
    pub prefix_frozen: bool,
}

// ---------------------------------------------------------------- helpers

/// Estimated summary size, used until a real summary exists.
pub fn summary_estimate(tokens: u32) -> u32 {
    (tokens / 5).clamp(24, 400).min(tokens)
}

/// Size of a node once summarized. Tool calls are never rewritten (their
/// arguments stay verbatim), so they do not shrink.
pub fn node_est(n: &PNode) -> u32 {
    if n.kind == NodeKind::ToolCall {
        n.tokens
    } else {
        n.summary_tokens.unwrap_or_else(|| summary_estimate(n.tokens)).min(n.tokens)
    }
}

/// Tokens a node costs under a decision.
pub fn planned_cost(n: &PNode, a: Action) -> u32 {
    match a {
        Action::Keep => n.tokens,
        Action::Summarize => node_est(n),
        Action::Drop => 0,
    }
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

/// Leading nodes that were part of the previous call: the prefix a provider
/// can serve from its prompt cache if we send it unchanged.
pub fn cache_prefix_len(nodes: &[PNode]) -> usize {
    nodes.iter().take_while(|n| n.prev.is_some()).count()
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
    let query =
        inp.nodes.iter().rev().find(|n| n.kind == NodeKind::User).map(|n| words(&n.content)).unwrap_or_default();
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
            let s =
                w.w_imp * n.importance.clamp(0.0, 1.0) + w.w_rec * recency + w.w_ref * ref_deg + w.w_task * relevance
                    - w.w_sup * sup;
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

fn verb(a: Action) -> &'static str {
    match a {
        Action::Keep => "Kept",
        Action::Summarize => "Summarized",
        Action::Drop => "Dropped",
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
    can_summarize: bool,
    /// Same decision last call for every member, if the whole group existed then.
    prev: Option<Action>,
    in_prefix: bool,
}

/// Decide keep / summarize / drop for each node within the token budget.
pub fn plan(inp: &PolicyInput) -> Plan {
    let nodes = &inp.nodes;
    let n = nodes.len();
    let w = &inp.weights;
    let scores = score_nodes(inp);
    let forced = apply_rules(nodes, inp.latest_turn, &inp.rules);
    let prefix_len = cache_prefix_len(nodes);
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
    for (i, slot) in group_of.iter_mut().enumerate() {
        let r = find(&mut parent, i);
        let g = *root_group.entry(r).or_insert_with(|| {
            members.push(Vec::new());
            members.len() - 1
        });
        members[g].push(i);
        *slot = g;
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
        } else if x.locked {
            Some("block type cannot be rewritten".into())
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
            let est: u32 = ms.iter().map(|&i| node_est(&nodes[i])).sum();
            let score = ms.iter().map(|&i| scores[i]).fold(f32::MIN, f32::max);
            let mut required = ms.iter().find_map(|&i| required_reason(i));
            if let (Some(r), true) = (&mut required, ms.len() > 1) {
                *r = format!("{r} (tool pair stays together)");
            }
            // A group can be summarized when every member that needs a summary can have one
            // and the result is actually smaller.
            let can_summarize = est < tokens
                && ms.iter().all(|&i| {
                    let x = &nodes[i];
                    x.kind == NodeKind::ToolCall || (!inp.no_summary.contains(&x.id) && node_est(x) < x.tokens)
                });
            // Safest forced action among members wins, so a pair never splits.
            let forced = ms.iter().filter_map(|&i| forced[i].clone()).max_by_key(|(a, _)| safety(*a));
            let prevs: Vec<Option<Action>> = ms.iter().map(|&i| nodes[i].prev).collect();
            let prev = match prevs.first().copied().flatten() {
                Some(p) if prevs.iter().all(|x| *x == Some(p)) => Some(p),
                _ => None,
            };
            let in_prefix = ms.iter().any(|&i| i < prefix_len);
            Group { members: ms, tokens, est, score, required, forced, can_summarize, prev, in_prefix }
        })
        .collect();

    let budget = inp.budget as u64;
    let mut decision: Vec<Option<(Action, String)>> = vec![None; groups.len()];
    let mut used: u64 = 0;

    // 1. Required groups and forced actions.
    for (g, grp) in groups.iter().enumerate() {
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
                Action::Summarize if grp.can_summarize => {
                    decision[g] = Some((Action::Summarize, format!("Summarized: {why}")));
                    used += grp.est as u64;
                }
                Action::Summarize => {} // no summary possible: let the budget decide keep vs drop
                Action::Drop => decision[g] = Some((Action::Drop, format!("Dropped: {why}"))),
            }
        }
    }
    let must_tokens = used;
    let over_budget = must_tokens > budget;

    // 2. Repeat last call's decisions for the unchanged prefix when they still fit
    //    (keeps the provider's prompt cache valid).
    let group_cost = |grp: &Group, a: Action| -> Option<u64> {
        Some(match a {
            Action::Keep => grp.tokens as u64,
            Action::Drop => 0,
            Action::Summarize => {
                // Only repeatable when the real summaries still exist.
                if !grp.can_summarize
                    || grp
                        .members
                        .iter()
                        .any(|&i| nodes[i].kind != NodeKind::ToolCall && nodes[i].summary_tokens.is_none())
                {
                    return None;
                }
                grp.est as u64
            }
        })
    };
    let frozen: Vec<(usize, Action, u64)> = (0..groups.len())
        .filter(|&g| decision[g].is_none() && groups[g].in_prefix)
        .filter_map(|g| {
            let p = groups[g].prev?;
            Some((g, p, group_cost(&groups[g], p)?))
        })
        .collect();
    let frozen_tokens: u64 = frozen.iter().map(|f| f.2).sum();
    let prefix_frozen = prefix_len > 0 && !over_budget && !frozen.is_empty() && used + frozen_tokens <= budget;
    if prefix_frozen {
        for &(g, a, cost) in &frozen {
            decision[g] = Some((a, format!("{}: same as last call (protects prompt cache)", verb(a))));
            used += cost;
        }
    }

    // 3. Greedy knapsack on score density for everything left.
    let mut rest: Vec<usize> = (0..groups.len()).filter(|&g| decision[g].is_none()).collect();
    rest.sort_by(|&a, &b| {
        let (da, db) =
            (groups[a].score / groups[a].tokens.max(1) as f32, groups[b].score / groups[b].tokens.max(1) as f32);
        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal).then(groups[a].members[0].cmp(&groups[b].members[0]))
    });
    let superseded_by: HashMap<&str, &str> =
        inp.edges.iter().filter(|e| e.kind == EdgeKind::Supersedes).map(|e| (e.to.as_str(), e.from.as_str())).collect();
    for g in rest {
        let grp = &mut groups[g];
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
        } else if grp.can_summarize && used + grp.est as u64 <= budget && grp.score > w.summarize_threshold {
            used += grp.est as u64;
            (
                Action::Summarize,
                format!("Summarized: no room in full; score {:.2} above {:.2}", grp.score, w.summarize_threshold),
            )
        } else if grp.score > w.summarize_threshold {
            (Action::Drop, format!("Dropped: no room even summarized (score {:.2}{sup})", grp.score))
        } else {
            (
                Action::Drop,
                format!("Dropped: score {:.2} below {:.2} and no room{sup}", grp.score, w.summarize_threshold),
            )
        });
    }

    // Assemble in request order.
    let items: Vec<PlanItem> = (0..n)
        .map(|i| {
            let g = group_of[i];
            let (action, reason) = decision[g].clone().expect("every group decided");
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
                planned_tokens: planned_cost(&nodes[i], action),
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
            || n.locked
            || Some(i) == last_user
            || (inp.provider == Provider::Anthropic && Some(i) == first_user);
        if must && item.decision != Action::Keep {
            return Err(format!("required node {} was {:?}", n.id, item.decision));
        }
        if item.decision == Action::Summarize && n.kind != NodeKind::ToolCall && inp.no_summary.contains(&n.id) {
            return Err(format!("node {} summarized without an available summary", n.id));
        }
        if item.planned_tokens != planned_cost(n, item.decision) {
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
        return Err(format!(
            "plan {} exceeds budget {} although the required set fits",
            plan.planned_tokens, plan.budget
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{build_edges, GNode};
    use proptest::prelude::*;

    fn node(id: &str, kind: NodeKind, tokens: u32, turn: u32, content: &str) -> PNode {
        PNode {
            id: id.into(),
            kind,
            content: content.into(),
            content_hash: [0; 32],
            tokens,
            first_seen_turn: turn,
            source: None,
            importance: 0.5,
            pinned: false,
            locked: false,
            user_action: None,
            summary_tokens: None,
            prev: None,
        }
    }

    /// Like the store: nodes that predate the latest turn were sent as-is last call.
    fn input(mut nodes: Vec<PNode>, edges: Vec<Edge>, budget: u32, latest: u32) -> PolicyInput {
        for n in nodes.iter_mut() {
            if n.prev.is_none() && n.first_seen_turn < latest {
                n.prev = Some(Action::Keep);
            }
        }
        PolicyInput {
            nodes,
            edges,
            latest_turn: latest,
            budget,
            weights: Weights::default(),
            rules: vec![],
            provider: Provider::Anthropic,
            no_summary: HashSet::new(),
        }
    }

    fn decisions(p: &Plan) -> Vec<(&str, Action)> {
        p.items.iter().map(|i| (i.id.as_str(), i.decision)).collect()
    }

    #[test]
    fn everything_is_kept_when_it_fits() {
        let nodes = vec![
            node("s", NodeKind::System, 10, 0, "sys"),
            node("u", NodeKind::User, 10, 0, "hi"),
            node("a", NodeKind::Assistant, 10, 1, "yo"),
        ];
        let inp = input(nodes, vec![], 1000, 1);
        let p = plan(&inp);
        assert!(p.items.iter().all(|i| i.decision == Action::Keep));
        assert!(validate_plan(&inp, &p).is_ok());
    }

    #[test]
    fn low_value_old_bulk_is_dropped_under_pressure() {
        let mut nodes =
            vec![node("s", NodeKind::System, 10, 0, "sys"), node("u0", NodeKind::User, 10, 0, "fix the parser")];
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
        let p = plan(&input(nodes, vec![], 150, 9));
        let item = explain(&p, "p").unwrap();
        assert_eq!(item.decision, Action::Keep);
        assert!(item.reason.contains("pinned") && item.reason.contains("ignored"), "{}", item.reason);
    }

    #[test]
    fn locked_blocks_are_never_rewritten() {
        let mut thinking = node("th", NodeKind::Assistant, 500, 0, "{thinking}");
        thinking.locked = true;
        thinking.user_action = Some(Action::Drop);
        let mut nodes = vec![node("s", NodeKind::System, 10, 0, "s"), node("u", NodeKind::User, 10, 0, "u"), thinking];
        for i in 0..5 {
            nodes.push(node(&format!("x{i}"), NodeKind::Assistant, 50, 9, "filler"));
        }
        let inp = input(nodes, vec![], 100, 9);
        let p = plan(&inp);
        assert_eq!(explain(&p, "th").unwrap().decision, Action::Keep);
        assert!(validate_plan(&inp, &p).is_ok());
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
    fn missing_summary_falls_back_to_keep_when_it_fits_else_drop() {
        let mut nodes = vec![
            node("s", NodeKind::System, 5, 0, "s"),
            node("u", NodeKind::User, 5, 0, "u"),
            node("big", NodeKind::ToolResult, 400, 0, "x"),
        ];
        for i in 0..4 {
            nodes.push(node(&format!("t{i}"), NodeKind::Assistant, 5, 10, "t"));
        }
        let rule = PolicyRule {
            id: "r".into(),
            session_id: None,
            matcher: Matcher { kind: Some(NodeKind::ToolResult), ..Default::default() },
            action: Action::Summarize,
            priority: 1,
        };
        let mut inp = input(nodes, vec![], 10_000, 10);
        inp.rules = vec![rule];
        assert_eq!(explain(&plan(&inp), "big").unwrap().decision, Action::Summarize);

        inp.no_summary.insert("big".into());
        let roomy = plan(&inp);
        assert_eq!(explain(&roomy, "big").unwrap().decision, Action::Keep, "fits, so keep");
        assert!(validate_plan(&inp, &roomy).is_ok());

        inp.budget = 100;
        let tight = plan(&inp);
        assert_eq!(explain(&tight, "big").unwrap().decision, Action::Drop, "no room, so drop");
        assert!(validate_plan(&inp, &tight).is_ok());
    }

    #[test]
    fn real_summary_size_is_used_for_budgeting() {
        let mut big = node("big", NodeKind::ToolResult, 1000, 0, "x");
        big.summary_tokens = Some(37);
        assert_eq!(node_est(&big), 37);
        big.summary_tokens = Some(5000); // a "summary" larger than the original is useless
        assert_eq!(node_est(&big), 1000);
        let call = node("c", NodeKind::ToolCall, 20, 0, "{}");
        assert_eq!(node_est(&call), 20, "tool calls stay verbatim");
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
    fn unchanged_prefix_repeats_last_decisions_when_they_fit() {
        let mut nodes = vec![node("s", NodeKind::System, 10, 0, "s"), node("u", NodeKind::User, 10, 0, "do it")];
        let mut dropped = node("junk", NodeKind::ToolResult, 100, 1, "zzz");
        dropped.prev = Some(Action::Drop); // we dropped it last call; it must stay dropped
        nodes.push(dropped);
        nodes.push(node("keepme", NodeKind::Assistant, 30, 1, "fine"));
        for i in 0..4 {
            nodes.push(node(&format!("t{i}"), NodeKind::Assistant, 10, 5, "t"));
        }
        let p = plan(&input(nodes, vec![], 1000, 5));
        assert!(p.prefix_frozen);
        let junk = explain(&p, "junk").unwrap();
        assert_eq!(junk.decision, Action::Drop, "repeating the old decision keeps the cached prefix valid");
        assert!(junk.reason.contains("same as last call"));
        assert_eq!(explain(&p, "keepme").unwrap().decision, Action::Keep);
    }

    #[test]
    fn prefix_is_released_when_the_budget_forces_edits() {
        let mut nodes = vec![node("s", NodeKind::System, 10, 0, "s"), node("u", NodeKind::User, 10, 0, "do it")];
        nodes.push(node("junk", NodeKind::ToolResult, 100, 1, "zzz"));
        for i in 0..4 {
            nodes.push(node(&format!("t{i}"), NodeKind::Assistant, 10, 5, "t"));
        }
        let inp = input(nodes, vec![], 80, 5);
        let tight = plan(&inp);
        assert!(!tight.prefix_frozen);
        assert!(tight.planned_tokens <= 80);
        assert!(validate_plan(&inp, &tight).is_ok());
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
            proptest::option::of(prop::sample::select(vec![
                NodeKind::ToolResult,
                NodeKind::File,
                NodeKind::Assistant,
                NodeKind::ToolCall,
            ])),
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

    /// Per-node random extras: (pin, importance, user action, locked, summary size, previous decision, no_summary).
    type Flags = (bool, u8, u8, bool, u16, u8, bool);

    /// Build a realistic request: system, user, then random steps, with real graph edges.
    fn build(
        steps: &[Step],
        flags: &[Flags],
        budget: u32,
        provider: Provider,
        rules: Vec<PolicyRule>,
        tail: usize,
    ) -> PolicyInput {
        let mut nodes = vec![
            node("n0", NodeKind::System, 40, 0, "system rules"),
            node("n1", NodeKind::User, 20, 0, "refactor the parser module"),
        ];
        let mut meta: Vec<(Option<String>, Option<String>)> = vec![(None, None), (None, None)]; // source, tool_id
        let mut turn = 0u32;
        let push = |nodes: &mut Vec<PNode>,
                    meta: &mut Vec<(Option<String>, Option<String>)>,
                    mut n: PNode,
                    source: Option<String>,
                    tid: Option<String>| {
            n.id = format!("n{}", nodes.len());
            n.source = source.clone();
            nodes.push(n);
            meta.push((source, tid));
        };
        for (i, s) in steps.iter().enumerate() {
            turn += (i % 2) as u32;
            match s {
                Step::User(t) => {
                    push(&mut nodes, &mut meta, node("", NodeKind::User, *t, turn, "parser module please"), None, None)
                }
                Step::Text(t) => push(
                    &mut nodes,
                    &mut meta,
                    node("", NodeKind::Assistant, *t, turn, "working on the parser"),
                    None,
                    None,
                ),
                Step::Tool { file, size } => {
                    let path = format!("/repo/f{file}.rs");
                    let tid = format!("t{i}");
                    push(
                        &mut nodes,
                        &mut meta,
                        node("", NodeKind::ToolCall, 15, turn, &format!("{{\"file_path\":\"{path}\"}}")),
                        Some("Read".into()),
                        Some(tid.clone()),
                    );
                    push(
                        &mut nodes,
                        &mut meta,
                        node("", NodeKind::File, *size, turn, "file body"),
                        Some(path),
                        Some(tid),
                    );
                }
            }
        }
        let mut no_summary = HashSet::new();
        for (i, n) in nodes.iter_mut().enumerate() {
            let (pin, imp, act, locked, summ, prev, nosum) = flags[i % flags.len()];
            n.pinned = pin && i % 7 == 0;
            n.locked = locked && i % 11 == 3;
            n.importance = imp as f32 / 255.0;
            n.user_action = match act % 8 {
                1 => Some(Action::Keep),
                2 => Some(Action::Summarize),
                3 => Some(Action::Drop),
                _ => None,
            };
            if summ % 3 == 0 {
                n.summary_tokens = Some(summ as u32 % 700);
            }
            if n.first_seen_turn < turn {
                n.prev = match prev % 4 {
                    0 => Some(Action::Drop),
                    1 => Some(Action::Summarize),
                    2 => None,
                    _ => Some(Action::Keep),
                };
            }
            if nosum && i % 5 == 0 {
                no_summary.insert(n.id.clone());
            }
        }
        let g: Vec<GNode> = nodes
            .iter()
            .zip(&meta)
            .map(|(n, (src, tid))| GNode {
                id: &n.id,
                kind: n.kind,
                source: src.as_deref(),
                tool_id: tid.as_deref(),
                text: Some(&n.content),
            })
            .collect();
        let all: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        let edges = build_edges(&g, &all);
        PolicyInput {
            nodes,
            edges,
            latest_turn: turn,
            budget,
            weights: Weights { protect_tail: tail, ..Weights::default() },
            rules,
            provider,
            no_summary,
        }
    }

    fn flags() -> impl Strategy<Value = Flags> {
        (any::<bool>(), any::<u8>(), any::<u8>(), any::<bool>(), any::<u16>(), any::<u8>(), any::<bool>())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]

        #[test]
        fn every_plan_is_valid(
            steps in prop::collection::vec(step(), 0..40),
            flags in prop::collection::vec(flags(), 1..12),
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
            let mut inp = build(&steps, &[(false, 128, 0, false, 1, 3, false)], u32::MAX / 2, Provider::Anthropic, vec![], 4);
            inp.nodes.iter_mut().for_each(|n| {
                n.user_action = None;
                n.prev = n.prev.map(|_| Action::Keep);
            });
            let p = plan(&inp);
            prop_assert!(p.items.iter().all(|i| i.decision == Action::Keep));
            prop_assert_eq!(p.planned_tokens, p.original_tokens);
        }
    }
}
