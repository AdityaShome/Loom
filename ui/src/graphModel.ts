// Turns the nodes of one session into the hub-and-spoke picture: a central session node,
// category hubs around it, and leaf pills under each hub. Pure functions, no React.
//
// Hubs come in two kinds. Content hubs group what is *in* the context (System, User, Tools,
// Assistant, Knowledge). Status hubs describe what Loom is doing with it (Plan, Budget, Cache,
// Controls, Waste) and only appear when there is something to say.

import type { Action, Edge, NodeRow, Plan, PlanItem, Rule, Turn } from "./api";
import { fmtTokens } from "./format";

export type HubId =
  | "system"
  | "user"
  | "tools"
  | "assistant"
  | "knowledge"
  | "controls"
  | "cache"
  | "plan"
  | "budget"
  | "waste";
export type IconName = "shield" | "user" | "wrench" | "chat" | "book" | "pulse" | "cpu" | "sliders" | "gauge" | "bolt" | "copy";

export type HubMeta = {
  title: string;
  color: string;
  icon: IconName;
  side: "left" | "right";
  /** Position among the hubs on its side, top to bottom. */
  order: number;
  /** Content hubs hold nodes of one kind of thing; status hubs describe the plan. */
  content: boolean;
  blurb: string;
};

export const HUB_META: Record<HubId, HubMeta> = {
  system: { title: "System", color: "#60a5fa", icon: "shield", side: "left", order: 0, content: true, blurb: "Instructions the agent was started with. Always kept." },
  user: { title: "User", color: "#2dd4bf", icon: "user", side: "left", order: 1, content: true, blurb: "What you asked the agent to do." },
  tools: { title: "Tools & Actions", color: "#f472b6", icon: "wrench", side: "left", order: 2, content: true, blurb: "Tool calls and what they returned. A call and its result always move together." },
  controls: { title: "Your Controls", color: "#facc15", icon: "sliders", side: "left", order: 3, content: false, blurb: "Nodes you pinned or forced, and the rules that apply to this session. These override the scoring." },
  cache: { title: "Prompt Cache", color: "#22d3ee", icon: "bolt", side: "left", order: 4, content: false, blurb: "Providers cache the unchanged start of a request. Loom repeats its earlier decisions there so that cache stays valid." },
  assistant: { title: "Assistant", color: "#a78bfa", icon: "chat", side: "right", order: 0, content: true, blurb: "The model's own replies in this conversation." },
  knowledge: { title: "Knowledge", color: "#34d399", icon: "book", side: "right", order: 1, content: true, blurb: "Files and memory entries the agent pulled into context." },
  plan: { title: "Context Plan", color: "#fb923c", icon: "pulse", side: "right", order: 2, content: false, blurb: "What Loom keeps, summarizes or drops to stay within the budget." },
  budget: { title: "Token Budget", color: "#818cf8", icon: "gauge", side: "right", order: 3, content: false, blurb: "The token allowance for one request, the part that cannot be cut, and what the plan saves." },
  waste: { title: "Reclaimable", color: "#fb7185", icon: "copy", side: "right", order: 4, content: false, blurb: "Older reads that a newer read of the same file has replaced. They are the first thing the plan drops." },
};

export const DECISION_COLOR: Record<Action, string> = { Keep: "#34d399", Summarize: "#fbbf24", Drop: "#f87171" };

export type Leaf = {
  id: string;
  label: string;
  sub?: string;
  nodeIds: string[];
  tokens: number;
  color: string;
  /** "+N more": selecting it selects the whole hub. */
  more?: boolean;
  /** Extra lines for the details panel (rules, explanations). */
  notes?: string[];
};

export type Hub = {
  id: HubId;
  title: string;
  subtitle: string;
  color: string;
  icon: IconName;
  side: "left" | "right";
  order: number;
  nodeIds: string[];
  tokens: number;
  leaves: Leaf[];
  notes: string[];
};

export type Model = {
  hubs: Hub[];
  /** Nodes that are part of the latest request. */
  nodes: NodeRow[];
  byId: Map<string, NodeRow>;
  planById: Map<string, PlanItem>;
  edges: Edge[];
  total: number;
  /** Tokens in context against the budget, for the gauge on the centre node. */
  gauge: { used: number; budget: number; ratio: number; tone: "good" | "warn" | "bad" };
};

export type Selection =
  | { kind: "center" }
  | { kind: "hub"; id: HubId }
  | { kind: "leaf"; hub: HubId; leaf: string }
  | { kind: "node"; id: string };

export const MAX_LEAVES = 3;

export function inContext(nodes: NodeRow[], latest: number): NodeRow[] {
  return nodes.filter((n) => n.last_seen_turn === latest && n.status !== "DroppedByAgent");
}

/** The content hub a node belongs to. */
export function hubOf(n: NodeRow): HubId {
  switch (n.kind) {
    case "System":
      return "system";
    case "User":
      return "user";
    case "Assistant":
      return "assistant";
    case "ToolCall":
    case "ToolResult":
      return "tools";
    default:
      return "knowledge"; // File, Memory, Summary
  }
}

export function shorten(text: string, max: number): string {
  const t = text.replace(/\s+/g, " ").trim();
  return t.length <= max ? t : t.slice(0, Math.max(1, max - 1)).trimEnd() + "…";
}

export function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

/** Short token counts for tight spaces: 940, 4.1k, 12k. */
export function compact(n: number): string {
  return n < 1000 ? String(n) : n < 10_000 ? `${(n / 1000).toFixed(1)}k` : `${Math.round(n / 1000)}k`;
}

/** "just now", "12s ago", "3 min ago", "2h ago". */
export function ago(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  if (s < 3) return "just now";
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  return `${Math.floor(s / 3600)}h ago`;
}

const sum = (ns: NodeRow[]) => ns.reduce((s, n) => s + n.tokens, 0);
const plural = (n: number, one: string, many = one + "s") => `${n} ${n === 1 ? one : many}`;

/** Show at most `MAX_LEAVES` pills; the rest collapse into "+N more". */
export function capLeaves(leaves: Leaf[], color: string, max = MAX_LEAVES): Leaf[] {
  if (leaves.length <= max) return leaves;
  const shown = leaves.slice(0, max - 1);
  const rest = leaves.slice(max - 1);
  return [
    ...shown,
    {
      id: "more",
      label: `+${rest.length} more`,
      sub: fmtTokens(rest.reduce((s, l) => s + l.tokens, 0)) + " tokens",
      nodeIds: rest.flatMap((l) => l.nodeIds),
      tokens: rest.reduce((s, l) => s + l.tokens, 0),
      color,
      more: true,
    },
  ];
}

/** Tool name for a call, or for a result via the call it answers. */
function toolName(n: NodeRow, callOf: Map<string, NodeRow>): string {
  if (n.kind === "ToolCall") return n.source ?? "tool";
  return callOf.get(n.id)?.source ?? n.source ?? "tool";
}

/** What actually happens to a node. Tool calls in a summarized pair stay verbatim, so they count as kept. */
export function decisionOf(n: NodeRow, planById: Map<string, PlanItem>): Action {
  const d = planById.get(n.id)?.decision ?? (n.status === "Summarized" ? "Summarize" : n.status === "Dropped" ? "Drop" : "Keep");
  return d === "Summarize" && n.kind === "ToolCall" ? "Keep" : d;
}

export function describeRule(r: Rule): string {
  const m = r.matcher;
  const when = [
    m.kind && `kind is ${m.kind}`,
    m.source_glob && `source matches ${m.source_glob}`,
    m.min_tokens != null && `over ${m.min_tokens} tokens`,
    m.older_than_turns != null && `older than ${m.older_than_turns} turns`,
  ]
    .filter(Boolean)
    .join(" and ");
  return `${when || "any node"} → ${r.action} (priority ${r.priority}${r.session_id ? "" : ", all sessions"})`;
}

type Ctx0 = { callOf: Map<string, NodeRow>; planById: Map<string, PlanItem> };

function contentLeaves(id: HubId, ns: NodeRow[], m: Ctx0): Leaf[] {
  const color = HUB_META[id].color;
  const byPos = [...ns].sort((a, b) => a.position - b.position);
  const leaf = (n: NodeRow, label: string, sub: string): Leaf => ({ id: n.id, label, sub, nodeIds: [n.id], tokens: n.tokens, color });

  switch (id) {
    case "system":
      return byPos.map((n, i) => leaf(n, byPos.length === 1 ? "System prompt" : `System prompt ${i + 1}`, `${fmtTokens(n.tokens)} tokens`));
    case "user":
    case "assistant": {
      // Latest first, then (for the user) the original request, then the heaviest of the rest.
      const last = byPos[byPos.length - 1];
      const first = byPos[0];
      const rest = byPos.slice(1, -1).sort((a, b) => b.tokens - a.tokens);
      const order = id === "user" ? [last, ...(first !== last ? [first] : []), ...rest] : [last, ...[...byPos.slice(0, -1)].reverse()];
      return order.map((n, i) =>
        leaf(n, shorten(n.preview || n.kind, 20), i === 0 ? "latest" : id === "user" && n === first ? "first request" : `turn ${n.first_seen_turn + 1}`),
      );
    }
    case "tools": {
      const groups = new Map<string, NodeRow[]>();
      for (const n of byPos) {
        const k = toolName(n, m.callOf);
        groups.set(k, [...(groups.get(k) ?? []), n]);
      }
      return [...groups.entries()]
        .map(([name, g]) => ({ name, g, tokens: sum(g) }))
        .sort((a, b) => b.tokens - a.tokens)
        .map(({ name, g, tokens }) => {
          const calls = g.filter((n) => n.kind === "ToolCall").length || g.length;
          return { id: `tool:${name}`, label: shorten(name, 20), sub: `${plural(calls, "call")} · ${fmtTokens(tokens)}`, nodeIds: g.map((n) => n.id), tokens, color };
        });
    }
    case "knowledge": {
      const groups = new Map<string, NodeRow[]>();
      for (const n of byPos) {
        const k = n.source ?? shorten(n.preview, 24);
        groups.set(k, [...(groups.get(k) ?? []), n]);
      }
      return [...groups.entries()]
        .map(([src, g]) => ({ src, g, tokens: sum(g) }))
        .sort((a, b) => b.tokens - a.tokens)
        .map(({ src, g, tokens }) => ({
          id: `src:${src}`,
          label: shorten(basename(src), 20),
          sub: `${plural(g.length, "read")} · ${fmtTokens(tokens)}`,
          nodeIds: g.map((n) => n.id),
          tokens,
          color,
        }));
    }
    default:
      return [];
  }
}

export type BuildInput = {
  nodes: NodeRow[];
  edges: Edge[];
  plan: Plan | null;
  mode: string;
  latest: number;
  rules?: Rule[];
  /** Effective token budget (0 when unknown). */
  budget?: number;
};

export function buildModel(input: BuildInput): Model {
  const nodes = inContext(input.nodes, input.latest).sort((a, b) => a.position - b.position);
  const ids = new Set(nodes.map((n) => n.id));
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const edges = input.edges.filter((e) => ids.has(e.from) && ids.has(e.to));
  const planItems = input.plan?.items ?? [];
  const planById = new Map(planItems.map((i) => [i.id, i]));
  const callOf = new Map<string, NodeRow>();
  for (const e of edges) if (e.kind === "ToolPair" && byId.has(e.from)) callOf.set(e.to, byId.get(e.from)!);
  const ctx0: Ctx0 = { callOf, planById };
  const plan = input.plan;
  const budget = input.budget ?? plan?.budget ?? 0;
  const rules = input.rules ?? [];
  const total = sum(nodes);

  const hubs: Hub[] = [];
  const add = (id: HubId, p: { subtitle: string; nodeIds: string[]; tokens: number; leaves: Leaf[]; notes?: string[] }) => {
    const meta = HUB_META[id];
    hubs.push({
      id,
      title: meta.title,
      subtitle: p.subtitle,
      color: meta.color,
      icon: meta.icon,
      side: meta.side,
      order: meta.order,
      nodeIds: p.nodeIds,
      tokens: p.tokens,
      leaves: capLeaves(p.leaves, meta.color),
      notes: p.notes ?? [],
    });
  };

  // ---- content hubs
  const groups = new Map<HubId, NodeRow[]>();
  for (const n of nodes) groups.set(hubOf(n), [...(groups.get(hubOf(n)) ?? []), n]);
  const t = (ns: NodeRow[]) => compact(sum(ns));
  for (const id of ["system", "user", "tools", "assistant", "knowledge"] as HubId[]) {
    const ns = groups.get(id) ?? [];
    if (!ns.length) continue;
    const subtitle = {
      system: `${plural(ns.length, "prompt")} · ${t(ns)} tok`,
      user: `${plural(ns.length, "message")} · ${t(ns)} tok`,
      assistant: `${plural(ns.length, "reply", "replies")} · ${t(ns)} tok`,
      tools: `${plural(ns.filter((n) => n.kind === "ToolCall").length, "call")} · ${t(ns)} tok`,
      knowledge: `${plural(new Set(ns.map((n) => n.source ?? n.id)).size, "source")} · ${t(ns)} tok`,
    }[id as "system" | "user" | "tools" | "assistant" | "knowledge"];
    add(id, { subtitle, nodeIds: ns.map((n) => n.id), tokens: sum(ns), leaves: contentLeaves(id, ns, ctx0) });
  }

  // ---- your controls: pins, forced actions, rules
  const pinned = nodes.filter((n) => n.pinned);
  const forced = nodes.filter((n) => n.user_action);
  if (pinned.length || forced.length || rules.length) {
    const leaves: Leaf[] = [];
    const color = HUB_META.controls.color;
    if (pinned.length) leaves.push({ id: "pinned", label: "Pinned", sub: `${plural(pinned.length, "node")} · ${fmtTokens(sum(pinned))}`, nodeIds: pinned.map((n) => n.id), tokens: sum(pinned), color });
    for (const a of ["Keep", "Summarize", "Drop"] as Action[]) {
      const f = forced.filter((n) => n.user_action === a);
      if (f.length) leaves.push({ id: `forced:${a}`, label: `Forced ${a}`, sub: `${plural(f.length, "node")} · ${fmtTokens(sum(f))}`, nodeIds: f.map((n) => n.id), tokens: sum(f), color });
    }
    if (rules.length) {
      leaves.push({ id: "rules", label: plural(rules.length, "rule"), sub: "apply to this session", nodeIds: [], tokens: 0, color, notes: rules.map(describeRule) });
    }
    const covered = [...new Set([...pinned, ...forced].map((n) => n.id))];
    add("controls", {
      subtitle: `${pinned.length} pinned · ${plural(rules.length, "rule")}`,
      nodeIds: covered,
      tokens: sum([...pinned, ...forced.filter((n) => !n.pinned)]),
      leaves,
      notes: rules.map(describeRule),
    });
  }

  // ---- prompt cache: the unchanged start of the request
  if (plan && nodes.length) {
    const prefix = nodes.slice(0, Math.min(plan.cache_prefix_len, nodes.length));
    const fresh = nodes.filter((n) => n.first_seen_turn === input.latest);
    const color = HUB_META.cache.color;
    const leaves: Leaf[] = [];
    if (prefix.length) leaves.push({ id: "prefix", label: "Unchanged start", sub: `${plural(prefix.length, "node")} · ${fmtTokens(sum(prefix))}`, nodeIds: prefix.map((n) => n.id), tokens: sum(prefix), color });
    if (fresh.length) leaves.push({ id: "fresh", label: "New this turn", sub: `${plural(fresh.length, "node")} · ${fmtTokens(sum(fresh))}`, nodeIds: fresh.map((n) => n.id), tokens: sum(fresh), color: "#e2e8f0" });
    leaves.push({
      id: "frozen",
      label: plan.prefix_frozen ? "Decisions repeated" : "Not held fixed",
      sub: plan.prefix_frozen ? "cache stays valid" : prefix.length ? "budget forces edits" : "nothing sent before",
      nodeIds: [],
      tokens: 0,
      color: plan.prefix_frozen ? "#34d399" : "#94a3b8",
      notes: [
        plan.prefix_frozen
          ? "Loom repeats what it did to the unchanged start of the request last call, so the provider's prompt cache keeps working."
          : prefix.length
            ? "The budget does not leave room to keep the old decisions, so the start of the request will change and the provider cache restarts."
            : "This is the first request of the session, so there is no earlier prefix to protect.",
      ],
    });
    add("cache", {
      subtitle: `${plan.cache_prefix_len} unchanged · ${fresh.length} new`,
      nodeIds: nodes.map((n) => n.id),
      tokens: sum(prefix),
      leaves,
    });
  }

  // ---- plan: what happens to each node
  if (nodes.length) {
    const by: Record<Action, NodeRow[]> = { Keep: [], Summarize: [], Drop: [] };
    for (const n of nodes) by[decisionOf(n, planById)].push(n);
    const names: Record<Action, string> = { Keep: "Kept", Summarize: "Summarized", Drop: "Dropped" };
    const leaves = (["Keep", "Summarize", "Drop"] as Action[])
      .filter((a) => by[a].length > 0)
      .map((a) => {
        const after = a === "Summarize" ? by[a].reduce((s, n) => s + (planById.get(n.id)?.planned_tokens ?? n.tokens), 0) : null;
        return {
          id: `decision:${a}`,
          label: names[a],
          sub: `${plural(by[a].length, "node")} · ${after != null ? `${compact(sum(by[a]))}→${compact(after)}` : fmtTokens(sum(by[a]))}`,
          nodeIds: by[a].map((n) => n.id),
          tokens: sum(by[a]),
          color: DECISION_COLOR[a],
        };
      });
    add("plan", {
      subtitle: `${by.Keep.length} keep · ${by.Summarize.length} sum · ${by.Drop.length} drop`,
      nodeIds: nodes.map((n) => n.id),
      tokens: plan?.planned_tokens ?? total,
      leaves,
    });
  }

  // ---- budget
  if (plan && nodes.length) {
    const over = plan.over_budget; // the policy's own flag is the single source of truth
    const required = nodes.filter((n) => planById.get(n.id)?.required);
    const saved = plan.original_tokens - plan.planned_tokens;
    const color = HUB_META.budget.color;
    const kept = nodes.filter((n) => decisionOf(n, planById) !== "Drop");
    const cut = nodes.filter((n) => decisionOf(n, planById) !== "Keep");
    add("budget", {
      subtitle: `${compact(plan.planned_tokens)} / ${compact(plan.budget)} tok`,
      nodeIds: nodes.map((n) => n.id),
      tokens: plan.planned_tokens,
      leaves: [
        { id: "floor", label: "Required floor", sub: `${fmtTokens(plan.must_tokens)} tokens`, nodeIds: required.map((n) => n.id), tokens: plan.must_tokens, color: over ? "#f87171" : color },
        { id: "planned", label: over ? "Over budget" : "Planned", sub: `${fmtTokens(plan.planned_tokens)} of ${fmtTokens(plan.budget)}`, nodeIds: kept.map((n) => n.id), tokens: plan.planned_tokens, color: over ? "#f87171" : "#34d399" },
        {
          id: "saved",
          label: saved > 0 ? "Saved" : "Nothing to save",
          sub: saved > 0 ? `${fmtTokens(saved)} · ${Math.round((saved / Math.max(1, plan.original_tokens)) * 100)}%` : "fits as is",
          nodeIds: cut.map((n) => n.id),
          tokens: Math.max(0, saved),
          color: saved > 0 ? "#fbbf24" : "#94a3b8",
        },
      ],
    });
  }

  // ---- reclaimable: older versions that a newer read replaced
  const supersededSet = new Set(edges.filter((e) => e.kind === "Supersedes").map((e) => e.to));
  const old = nodes.filter((n) => supersededSet.has(n.id)).sort((a, b) => b.tokens - a.tokens);
  if (old.length) {
    const newerOf = new Map(edges.filter((e) => e.kind === "Supersedes").map((e) => [e.to, e.from]));
    add("waste", {
      subtitle: `${plural(old.length, "older read")} · ${compact(sum(old))} tok`,
      nodeIds: old.map((n) => n.id),
      tokens: sum(old),
      leaves: old.map((n) => ({
        id: n.id,
        label: shorten(n.source ? basename(n.source) : n.preview || n.kind, 20),
        sub: `replaced${byId.get(newerOf.get(n.id) ?? "") ? ` by turn ${byId.get(newerOf.get(n.id)!)!.first_seen_turn + 1}` : ""} · ${fmtTokens(n.tokens)}`,
        nodeIds: [n.id],
        tokens: n.tokens,
        color: HUB_META.waste.color,
      })),
    });
  }

  const gaugeBudget = budget || plan?.budget || 0;
  const ratio = gaugeBudget ? total / gaugeBudget : 0;
  return {
    hubs: hubs.sort((a, b) => a.order - b.order),
    nodes,
    byId,
    planById,
    edges,
    total,
    gauge: { used: total, budget: gaugeBudget, ratio, tone: ratio > 1 ? "bad" : ratio > 0.8 ? "warn" : "good" },
  };
}

// ------------------------------------------------------------------ insights

export type Insight = { text: string; tone?: "good" | "warn" | "bad" };
export type Ctx = { budget: number; plan: Plan | null; mode: string; latest: number };

function supersededIds(m: Model): Set<string> {
  return new Set(m.edges.filter((e) => e.kind === "Supersedes").map((e) => e.to));
}

function largest(ns: NodeRow[]): NodeRow | undefined {
  return [...ns].sort((a, b) => b.tokens - a.tokens)[0];
}

function nodeLabel(n: NodeRow): string {
  return n.source ? basename(n.source) : shorten(n.preview || n.kind, 28);
}

export function centerInsights(m: Model, c: Ctx): Insight[] {
  const out: Insight[] = [];
  const pct = c.budget ? Math.round((m.total / c.budget) * 100) : 0;
  out.push({
    text: `${fmtTokens(m.total)} tokens in the latest request (${pct}% of the ${fmtTokens(c.budget)} budget)`,
    tone: m.total > c.budget ? "bad" : "good",
  });
  if (c.plan) {
    const saved = c.plan.original_tokens - c.plan.planned_tokens;
    if (saved > 0) {
      out.push({
        text:
          c.mode === "Enforce"
            ? `Enforce is saving ${fmtTokens(saved)} tokens (${Math.round((saved / Math.max(1, c.plan.original_tokens)) * 100)}%) on this call`
            : `The plan would save ${fmtTokens(saved)} tokens; ${c.mode} still sends the original`,
        tone: "good",
      });
    } else out.push({ text: "Everything fits the budget; nothing needs to change", tone: "good" });
    if (c.plan.over_budget) out.push({ text: "Required items alone exceed the budget", tone: "bad" });
  }
  const pinned = m.nodes.filter((n) => n.pinned);
  if (pinned.length) out.push({ text: `${plural(pinned.length, "node")} pinned (${fmtTokens(sum(pinned))} tokens sent every call)` });
  const big = largest(m.nodes);
  if (big) out.push({ text: `Largest item: ${nodeLabel(big)} (${fmtTokens(big.tokens)} tokens)`, tone: big.tokens > m.total * 0.4 ? "warn" : undefined });
  const sup = supersededIds(m);
  if (sup.size) out.push({ text: `${plural(sup.size, "older read")} superseded by newer ones`, tone: "warn" });
  return out.slice(0, 5);
}

export function hubInsights(h: Hub, m: Model, c?: Ctx): Insight[] {
  const ns = h.nodeIds.map((id) => m.byId.get(id)!).filter(Boolean);
  const out: Insight[] = [];
  const plan = c?.plan ?? null;
  switch (h.id) {
    case "plan": {
      out.push({ text: h.subtitle });
      for (const l of h.leaves) out.push({ text: `${l.label}: ${l.sub}`, tone: l.label === "Dropped" ? "warn" : "good" });
      return out;
    }
    case "budget": {
      if (!plan) return [{ text: "No plan yet" }];
      out.push({ text: `Budget ${fmtTokens(plan.budget)} tokens; the plan uses ${fmtTokens(plan.planned_tokens)}`, tone: plan.over_budget ? "bad" : "good" });
      out.push({ text: `Required floor: ${fmtTokens(plan.must_tokens)} tokens that cannot be removed (system prompt, pins, latest message, recent context)` });
      const saved = plan.original_tokens - plan.planned_tokens;
      out.push({ text: saved > 0 ? `${fmtTokens(saved)} tokens saved (${Math.round((saved / Math.max(1, plan.original_tokens)) * 100)}%)` : "Nothing needs to be cut", tone: saved > 0 ? "good" : undefined });
      if (plan.over_budget) out.push({ text: "Required items alone exceed the budget: unpin something or raise it", tone: "bad" });
      return out;
    }
    case "cache": {
      if (!plan) return [];
      out.push({ text: `${plural(plan.cache_prefix_len, "node")} at the start were sent unchanged last call` });
      out.push({
        text: plan.prefix_frozen ? "Their decisions are repeated, so the provider's prompt cache stays valid" : "Not held fixed: nothing to protect, or the budget forces edits",
        tone: plan.prefix_frozen ? "good" : "warn",
      });
      return out;
    }
    case "controls": {
      const pinned = ns.filter((n) => n.pinned);
      if (pinned.length) out.push({ text: `${plural(pinned.length, "node")} pinned: ${fmtTokens(sum(pinned))} tokens sent on every call` });
      const forced = ns.filter((n) => n.user_action);
      if (forced.length) out.push({ text: `${plural(forced.length, "node")} with a forced action` });
      for (const note of h.notes.slice(0, 3)) out.push({ text: note });
      return out.length ? out : [{ text: "Nothing pinned or forced" }];
    }
    case "waste": {
      out.push({ text: `${fmtTokens(sum(ns))} tokens sit in ${plural(ns.length, "older read")} that newer reads replaced`, tone: "warn" });
      out.push({ text: "The plan drops these first when the budget is tight" });
      return out;
    }
    default:
      break;
  }
  const share = m.total ? Math.round((sum(ns) / m.total) * 100) : 0;
  out.push({ text: `${plural(ns.length, "node")}, ${fmtTokens(sum(ns))} tokens (${share}% of the context)` });
  const big = largest(ns);
  if (big && ns.length > 1) out.push({ text: `Largest: ${nodeLabel(big)} (${fmtTokens(big.tokens)} tokens)` });
  const counts: Record<Action, number> = { Keep: 0, Summarize: 0, Drop: 0 };
  ns.forEach((n) => counts[decisionOf(n, m.planById)]++);
  if (counts.Summarize + counts.Drop > 0) {
    out.push({ text: `Plan: keep ${counts.Keep}, summarize ${counts.Summarize}, drop ${counts.Drop}`, tone: "warn" });
  } else out.push({ text: "Plan keeps all of it", tone: "good" });
  const pinned = ns.filter((n) => n.pinned).length;
  if (pinned) out.push({ text: `${plural(pinned, "node")} pinned` });
  const sup = supersededIds(m);
  const stale = ns.filter((n) => sup.has(n.id)).length;
  if (stale) out.push({ text: `${plural(stale, "node")} superseded by a newer version`, tone: "warn" });
  return out.slice(0, 5);
}

export function nodeInsights(n: NodeRow, m: Model): Insight[] {
  const out: Insight[] = [];
  const item = m.planById.get(n.id);
  out.push({ text: `${fmtTokens(n.tokens)} tokens · seen in turns ${n.first_seen_turn + 1}–${n.last_seen_turn + 1}` });
  if (item) out.push({ text: `${item.decision}: ${item.reason}`, tone: item.decision === "Keep" ? "good" : "warn" });
  if (n.pinned) out.push({ text: "Pinned: always sent" });
  if (n.user_action) out.push({ text: `You forced: ${n.user_action}` });
  if (supersededIds(m).has(n.id)) out.push({ text: "A newer version of this exists", tone: "warn" });
  const rel = m.edges.filter((e) => (e.from === n.id || e.to === n.id) && e.kind !== "Follows").length;
  if (rel) out.push({ text: `${plural(rel, "relation")} to other nodes` });
  return out.slice(0, 5);
}

// ------------------------------------------------------------------ related nodes

export type Related = { key: string; label: string; color: string; icon: IconName; value: string; target: Selection };

const nodeIcon = (n: NodeRow): IconName => HUB_META[hubOf(n)].icon;
const nodeColor = (n: NodeRow): string => HUB_META[hubOf(n)].color;

function relatedNode(n: NodeRow, value: string): Related {
  return { key: n.id, label: nodeLabel(n), color: nodeColor(n), icon: nodeIcon(n), value, target: { kind: "node", id: n.id } };
}

export function related(sel: Selection, m: Model): Related[] {
  switch (sel.kind) {
    case "center":
      return m.hubs
        .filter((h) => HUB_META[h.id].content)
        .map((h) => ({
          key: h.id,
          label: h.title,
          color: h.color,
          icon: h.icon,
          value: `${m.total ? Math.round((h.tokens / m.total) * 100) : 0}%`,
          target: { kind: "hub", id: h.id } as Selection,
        }));
    case "hub": {
      const h = m.hubs.find((x) => x.id === sel.id);
      if (!h) return [];
      const bySize = h.id === "budget" || h.id === "cache" || h.id === "waste";
      return h.nodeIds
        .map((id) => m.byId.get(id)!)
        .filter(Boolean)
        .sort((a, b) => (bySize ? b.tokens - a.tokens : (m.planById.get(b.id)?.score ?? 0) - (m.planById.get(a.id)?.score ?? 0)))
        .slice(0, 6)
        .map((n) => relatedNode(n, bySize ? `${fmtTokens(n.tokens)} tok` : (m.planById.get(n.id)?.score ?? n.score).toFixed(2)));
    }
    case "leaf": {
      const l = m.hubs.find((x) => x.id === sel.hub)?.leaves.find((x) => x.id === sel.leaf);
      return (l?.nodeIds ?? [])
        .map((id) => m.byId.get(id)!)
        .filter(Boolean)
        .slice(0, 8)
        .map((n) => relatedNode(n, `${fmtTokens(n.tokens)} tok`));
    }
    case "node": {
      const out: Related[] = [];
      for (const e of m.edges) {
        if (e.kind === "Follows") continue;
        const other = e.from === sel.id ? m.byId.get(e.to) : e.to === sel.id ? m.byId.get(e.from) : undefined;
        if (other) out.push(relatedNode(other, e.kind));
      }
      return out.slice(0, 8);
    }
  }
}

/** Nodes covered by a selection. */
export function nodesOf(sel: Selection, m: Model): NodeRow[] {
  const get = (ids: string[]) => ids.map((id) => m.byId.get(id)).filter((n): n is NodeRow => !!n);
  switch (sel.kind) {
    case "center":
      return m.nodes;
    case "hub":
      return get(m.hubs.find((h) => h.id === sel.id)?.nodeIds ?? []);
    case "leaf":
      return get(m.hubs.find((h) => h.id === sel.hub)?.leaves.find((l) => l.id === sel.leaf)?.nodeIds ?? []);
    case "node":
      return get([sel.id]);
  }
}

/** Tokens first seen per turn for a selection: the History tab. */
export function historyOf(ns: NodeRow[]): { turn: number; tokens: number; nodes: number }[] {
  const by = new Map<number, { tokens: number; nodes: number }>();
  for (const n of ns) {
    const e = by.get(n.first_seen_turn) ?? { tokens: 0, nodes: 0 };
    by.set(n.first_seen_turn, { tokens: e.tokens + n.tokens, nodes: e.nodes + 1 });
  }
  return [...by.entries()].sort((a, b) => a[0] - b[0]).map(([turn, v]) => ({ turn, ...v }));
}

/** Edges between categories (and within a node's neighbourhood) for the Relations tab. */
export function relationRows(sel: Selection, m: Model): { text: string; color: string }[] {
  if (sel.kind === "node" || sel.kind === "leaf") {
    const ids = new Set(nodesOf(sel, m).map((n) => n.id));
    return m.edges
      .filter((e) => e.kind !== "Follows" && (ids.has(e.from) || ids.has(e.to)))
      .slice(0, 12)
      .map((e) => {
        const a = m.byId.get(e.from);
        const b = m.byId.get(e.to);
        return { text: `${a ? nodeLabel(a) : "?"} → ${b ? nodeLabel(b) : "?"}  (${e.kind})`, color: a ? nodeColor(a) : "#888" };
      });
  }
  const within = sel.kind === "hub" ? new Set(nodesOf(sel, m).map((n) => n.id)) : null;
  const counts = new Map<string, { n: number; color: string }>();
  for (const e of m.edges) {
    if (e.kind === "Follows") continue;
    const a = m.byId.get(e.from);
    const b = m.byId.get(e.to);
    if (!a || !b) continue;
    if (within && !within.has(a.id) && !within.has(b.id)) continue;
    const key = `${HUB_META[hubOf(a)].title} → ${HUB_META[hubOf(b)].title}  (${e.kind})`;
    counts.set(key, { n: (counts.get(key)?.n ?? 0) + 1, color: nodeColor(a) });
  }
  return [...counts.entries()].map(([k, v]) => ({ text: `${k} ×${v.n}`, color: v.color }));
}

// ------------------------------------------------------------------ flow strip

export type FlowStep = { key: string; title: string; detail: string; color: string; icon: IconName };

export function flowSteps(m: Model, c: Ctx, turns: Turn[]): FlowStep[] {
  const last = turns[turns.length - 1];
  const sent = last?.sent_tokens ?? m.total;
  const agent = last?.request_tokens ?? m.total;
  const fresh = m.nodes.filter((n) => n.first_seen_turn === c.latest).length;
  const counts: Record<Action, number> = { Keep: 0, Summarize: 0, Drop: 0 };
  m.nodes.forEach((n) => counts[decisionOf(n, m.planById)]++);
  return [
    { key: "agent", title: "Agent sent", detail: `${fmtTokens(agent)} tokens · ${plural(m.nodes.length, "node")}`, color: "#2dd4bf", icon: "user" },
    { key: "ingest", title: "Ingest & link", detail: `${fresh} new · ${m.edges.filter((e) => e.kind !== "Follows").length} relations`, color: "#a78bfa", icon: "book" },
    { key: "plan", title: "Score & plan", detail: `keep ${counts.Keep} · summarize ${counts.Summarize} · drop ${counts.Drop}`, color: "#60a5fa", icon: "pulse" },
    {
      key: "rewrite",
      title: c.mode === "Enforce" ? "Rewrite" : "Preview",
      detail: c.mode === "Enforce" ? (last?.rewritten ? "request rewritten" : "unchanged") : `${c.mode}: original is sent`,
      color: "#fb923c",
      icon: "wrench",
    },
    { key: "model", title: "Model received", detail: `${fmtTokens(sent)} tokens`, color: "#34d399", icon: "cpu" },
  ];
}

/** Share of tokens saved across all turns, 0..100. */
export function savedPercent(turns: Turn[]): number {
  const orig = turns.reduce((s, t) => s + t.request_tokens, 0);
  const sent = turns.reduce((s, t) => s + t.sent_tokens, 0);
  return orig > 0 ? Math.round(((orig - sent) / orig) * 100) : 0;
}
