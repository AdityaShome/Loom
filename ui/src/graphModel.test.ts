import { describe, expect, it } from "vitest";
import type { Edge, NodeRow, Plan } from "./api";
import {
  buildModel,
  capLeaves,
  centerInsights,
  compact,
  decisionOf,
  flowSteps,
  historyOf,
  hubInsights,
  hubOf,
  inContext,
  nodesOf,
  related,
  savedPercent,
  shorten,
  type Leaf,
} from "./graphModel";

let seq = 0;
function node(p: Partial<NodeRow> & Pick<NodeRow, "kind">): NodeRow {
  seq += 1;
  return {
    id: p.id ?? `n${seq}`,
    role: "user",
    tokens: 10,
    first_seen_turn: 0,
    last_seen_turn: 2,
    position: seq,
    source: null,
    importance: 0.5,
    pinned: false,
    user_action: null,
    status: "Full",
    score: 1,
    preview: p.kind,
    ...p,
  };
}

/** system, user, read_file call+result (README), read_file call+result (a.rs), bash call+result, assistant, user. */
function session() {
  seq = 0;
  const nodes = [
    node({ id: "sys", kind: "System", preview: "You are a coding agent", tokens: 20 }),
    node({ id: "u1", kind: "User", preview: "Fix the parser bug", tokens: 15 }),
    node({ id: "c1", kind: "ToolCall", source: "read_file", preview: '{"path":"README.md"}', tokens: 9 }),
    node({ id: "r1", kind: "File", source: "/repo/README.md", preview: "# Loom", tokens: 1400 }),
    node({ id: "c2", kind: "ToolCall", source: "read_file", preview: '{"path":"src/a.rs"}', tokens: 9 }),
    node({ id: "r2", kind: "File", source: "/repo/src/a.rs", preview: "fn a() {}", tokens: 700 }),
    node({ id: "c3", kind: "ToolCall", source: "bash", preview: '{"cmd":"ls"}', tokens: 6 }),
    node({ id: "r3", kind: "ToolResult", source: "bash", preview: "a b c", tokens: 30 }),
    node({ id: "as", kind: "Assistant", preview: "The bug is in a.rs", tokens: 40, first_seen_turn: 1 }),
    node({ id: "u2", kind: "User", preview: "ok now fix it please", tokens: 12, first_seen_turn: 2 }),
    // An old node the agent stopped sending: must not appear.
    node({ id: "gone", kind: "Assistant", last_seen_turn: 1, status: "DroppedByAgent" }),
  ];
  const edges: Edge[] = [
    { from: "c1", to: "r1", kind: "ToolPair", weight: 1 },
    { from: "c2", to: "r2", kind: "ToolPair", weight: 1 },
    { from: "c3", to: "r3", kind: "ToolPair", weight: 1 },
    { from: "as", to: "r2", kind: "RefersTo", weight: 1 },
    { from: "r2", to: "r1", kind: "Supersedes", weight: 1 },
    { from: "u1", to: "sys", kind: "Follows", weight: 1 },
  ];
  return { nodes, edges };
}

const plan = (over: Partial<Plan> = {}): Plan => ({
  budget: 1000,
  items: [
    { id: "r1", decision: "Summarize", reason: "no room", score: 0.9, tokens: 1400, planned_tokens: 280, required: false },
    { id: "r2", decision: "Keep", reason: "fits", score: 1.4, tokens: 700, planned_tokens: 700, required: false },
  ],
  original_tokens: 2241,
  planned_tokens: 1121,
  must_tokens: 100,
  over_budget: false,
  cache_prefix_len: 0,
  prefix_frozen: false,
  ...over,
});

describe("hub assignment", () => {
  it("maps node kinds to hubs", () => {
    const k = (kind: string) => hubOf(node({ kind }));
    expect([k("System"), k("User"), k("Assistant"), k("ToolCall"), k("ToolResult"), k("File"), k("Memory")]).toEqual([
      "system",
      "user",
      "assistant",
      "tools",
      "tools",
      "knowledge",
      "knowledge",
    ]);
  });

  it("only nodes of the latest request are in context", () => {
    const { nodes } = session();
    const ids = inContext(nodes, 2).map((n) => n.id);
    expect(ids).not.toContain("gone");
    expect(ids).toHaveLength(10);
  });
});

describe("model", () => {
  it("builds the hubs that have content and totals tokens", () => {
    const { nodes, edges } = session();
    const m = buildModel({ nodes, edges, plan: plan(), mode: "Suggest", latest: 2 });
    expect(m.hubs.map((h) => h.id).sort()).toEqual(["assistant", "knowledge", "plan", "system", "tools", "user"]);
    expect(m.total).toBe(20 + 15 + 9 + 1400 + 9 + 700 + 6 + 30 + 40 + 12);
    expect(m.edges.every((e) => m.byId.has(e.from) && m.byId.has(e.to))).toBe(true);
  });

  it("hides hubs with nothing in them", () => {
    const { nodes, edges } = session();
    const onlyUser = nodes.filter((n) => n.kind === "User");
    const m = buildModel({ nodes: onlyUser, edges, plan: null, mode: "Observe", latest: 2 });
    expect(m.hubs.map((h) => h.id).sort()).toEqual(["plan", "user"]);
    expect(buildModel({ nodes: [], edges: [], plan: null, mode: "Observe", latest: 0 }).hubs).toEqual([]);
  });

  it("groups tool calls and results by tool name", () => {
    const { nodes, edges } = session();
    const m = buildModel({ nodes, edges, plan: null, mode: "Observe", latest: 2 });
    const tools = m.hubs.find((h) => h.id === "tools")!;
    const names = tools.leaves.map((l) => l.label).sort();
    expect(names).toEqual(["bash", "read_file"]);
    const read = tools.leaves.find((l) => l.label === "read_file")!;
    // File results live in the Knowledge hub; the Tools hub holds the calls (and non-file results).
    expect(read.nodeIds).toEqual(["c1", "c2"]);
    expect(tools.leaves.find((l) => l.label === "bash")!.nodeIds).toEqual(["c3", "r3"]);
    expect(read.sub).toContain("2 calls");
  });

  it("groups knowledge by source file, heaviest first", () => {
    const { nodes, edges } = session();
    const m = buildModel({ nodes, edges, plan: null, mode: "Observe", latest: 2 });
    const k = m.hubs.find((h) => h.id === "knowledge")!;
    expect(k.leaves.map((l) => l.label)).toEqual(["README.md", "a.rs"]);
    expect(k.subtitle).toContain("2 sources");
  });

  it("orders user leaves latest, then the first request", () => {
    const { nodes, edges } = session();
    const m = buildModel({ nodes, edges, plan: null, mode: "Observe", latest: 2 });
    const u = m.hubs.find((h) => h.id === "user")!;
    expect(u.leaves.map((l) => l.sub)).toEqual(["latest", "first request"]);
    expect(u.leaves[0].label).toBe("ok now fix it please");
  });

  it("plan hub shows kept / summarized / dropped from the plan", () => {
    const { nodes, edges } = session();
    const m = buildModel({ nodes, edges, plan: plan(), mode: "Enforce", latest: 2 });
    const p = m.hubs.find((h) => h.id === "plan")!;
    const summarized = p.leaves.find((l) => l.label === "Summarized")!;
    expect(summarized.nodeIds).toEqual(["r1"]);
    expect(p.leaves.map((l) => l.label)).toEqual(["Kept", "Summarized"]);
    expect(p.subtitle).toBe("1.1k / 1.0k tok");
  });
});

describe("effective decisions", () => {
  it("a tool call in a summarized pair counts as kept (its arguments stay verbatim)", () => {
    const call = node({ kind: "ToolCall", id: "c" });
    const result = node({ kind: "File", id: "r" });
    const planById = new Map([
      ["c", { id: "c", decision: "Summarize" as const, reason: "", score: 1, tokens: 9, planned_tokens: 9, required: false }],
      ["r", { id: "r", decision: "Summarize" as const, reason: "", score: 1, tokens: 900, planned_tokens: 200, required: false }],
    ]);
    expect(decisionOf(call, planById)).toBe("Keep");
    expect(decisionOf(result, planById)).toBe("Summarize");
  });

  it("compact formats short token counts", () => {
    expect([compact(940), compact(1000), compact(4115), compact(12_400)]).toEqual(["940", "1.0k", "4.1k", "12k"]);
  });
});

describe("leaf capping", () => {
  const leaf = (i: number): Leaf => ({ id: `l${i}`, label: `leaf ${i}`, nodeIds: [`n${i}`], tokens: 100, color: "#fff" });

  it("keeps up to three pills and folds the rest into '+N more'", () => {
    expect(capLeaves([leaf(1), leaf(2), leaf(3)], "#fff")).toHaveLength(3);
    const capped = capLeaves([1, 2, 3, 4, 5].map(leaf), "#fff");
    expect(capped.map((l) => l.label)).toEqual(["leaf 1", "leaf 2", "+3 more"]);
    expect(capped[2].more).toBe(true);
    expect(capped[2].nodeIds).toEqual(["n3", "n4", "n5"]);
    expect(capped[2].tokens).toBe(300);
  });
});

describe("insights and related", () => {
  it("center insights report budget use, savings, pins and superseded reads", () => {
    const { nodes, edges } = session();
    nodes.find((n) => n.id === "sys")!.pinned = true;
    const m = buildModel({ nodes, edges, plan: plan(), mode: "Suggest", latest: 2 });
    const text = centerInsights(m, { budget: 1000, plan: plan(), mode: "Suggest", latest: 2 }).map((i) => i.text);
    expect(text[0]).toContain("2,241 tokens");
    expect(text[0]).toContain("224%");
    expect(text.some((t) => t.includes("would save 1,120 tokens") && t.includes("Suggest"))).toBe(true);
    expect(text.some((t) => t.includes("1 node pinned"))).toBe(true);
    expect(text.some((t) => t.includes("Largest item: README.md"))).toBe(true);
  });

  it("says Enforce is saving when it is", () => {
    const { nodes, edges } = session();
    const m = buildModel({ nodes, edges, plan: plan(), mode: "Enforce", latest: 2 });
    const text = centerInsights(m, { budget: 1000, plan: plan(), mode: "Enforce", latest: 2 }).map((i) => i.text);
    expect(text.some((t) => t.startsWith("Enforce is saving"))).toBe(true);
  });

  it("hub insights count superseded nodes and decisions", () => {
    const { nodes, edges } = session();
    const m = buildModel({ nodes, edges, plan: plan(), mode: "Suggest", latest: 2 });
    const k = m.hubs.find((h) => h.id === "knowledge")!;
    const text = hubInsights(k, m).map((i) => i.text);
    expect(text.some((t) => t.includes("summarize 1"))).toBe(true);
    expect(text.some((t) => t.includes("superseded"))).toBe(true);
  });

  it("related nodes: center lists hubs by share, a node lists its relations", () => {
    const { nodes, edges } = session();
    const m = buildModel({ nodes, edges, plan: plan(), mode: "Suggest", latest: 2 });
    const centre = related({ kind: "center" }, m);
    expect(centre.map((r) => r.label)).toContain("Knowledge");
    expect(centre.find((r) => r.label === "Knowledge")!.value).toBe("94%");
    const forFile = related({ kind: "node", id: "r2" }, m);
    expect(forFile.map((r) => r.value).sort()).toEqual(["RefersTo", "Supersedes", "ToolPair"]);
    expect(related({ kind: "node", id: "sys" }, m)).toEqual([]); // only a Follows edge
  });

  it("nodesOf resolves every selection kind", () => {
    const { nodes, edges } = session();
    const m = buildModel({ nodes, edges, plan: null, mode: "Observe", latest: 2 });
    expect(nodesOf({ kind: "center" }, m)).toHaveLength(10);
    expect(nodesOf({ kind: "hub", id: "user" }, m).map((n) => n.id)).toEqual(["u1", "u2"]);
    expect(nodesOf({ kind: "leaf", hub: "knowledge", leaf: "src:/repo/src/a.rs" }, m).map((n) => n.id)).toEqual(["r2"]);
    expect(nodesOf({ kind: "node", id: "nope" }, m)).toEqual([]);
  });
});

describe("history and flow", () => {
  it("history sums tokens by the turn a node first appeared", () => {
    const { nodes } = session();
    const h = historyOf(nodes.filter((n) => n.id !== "gone"));
    expect(h.map((x) => x.turn)).toEqual([0, 1, 2]);
    expect(h[1]).toEqual({ turn: 1, tokens: 40, nodes: 1 });
  });

  it("flow steps describe what the agent sent and the model received", () => {
    const { nodes, edges } = session();
    const m = buildModel({ nodes, edges, plan: plan(), mode: "Enforce", latest: 2 });
    const turns = [
      { idx: 0, request_tokens: 100, sent_tokens: 100, rewritten: false, created_at: 0, nodes: 3 },
      { idx: 1, request_tokens: 2241, sent_tokens: 1121, rewritten: true, created_at: 0, nodes: 10 },
    ];
    const steps = flowSteps(m, { budget: 1000, plan: plan(), mode: "Enforce", latest: 2 }, turns);
    expect(steps.map((s) => s.key)).toEqual(["agent", "ingest", "plan", "rewrite", "model"]);
    expect(steps[0].detail).toContain("2,241 tokens");
    expect(steps[3].detail).toBe("request rewritten");
    expect(steps[4].detail).toBe("1,121 tokens");
    expect(savedPercent(turns)).toBe(48);
    expect(savedPercent([])).toBe(0);
  });

  it("shorten collapses whitespace and truncates", () => {
    expect(shorten("  a   b\n c ", 20)).toBe("a b c");
    expect(shorten("abcdefghij", 5)).toBe("abcd…");
  });
});
