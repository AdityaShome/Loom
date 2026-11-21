import { describe, expect, it } from "vitest";
import type { Entity, Fact, Knowledge } from "./api";
import {
  buildElements,
  byId,
  counts,
  estimateTokens,
  factSummary,
  factsAbout,
  groupByKind,
  NO_FILTER,
  shortName,
  visibleEntities,
} from "./knowledgeModel";

const ent = (id: string, kind: Entity["kind"], name: string, source: Entity["source"] = "auto"): Entity => ({
  id,
  session_id: "s",
  kind,
  name,
  note: null,
  source,
});
const fact = (id: string, subject_id: string, predicate: string, p: Partial<Fact> = {}): Fact => ({
  id,
  session_id: "s",
  subject_id,
  predicate,
  object_id: null,
  text: "",
  source: "auto",
  node_id: null,
  inject: false,
  ...p,
});

function sample(): Knowledge {
  return {
    entities: [
      ent("goal", "Goal", "Run the tests and fix the failing one"),
      ent("file", "File", "src/math.js"),
      ent("avg", "Symbol", "average"),
      ent("add", "Symbol", "add"),
      ent("cmd", "Command", "npm test"),
      ent("err", "Error", "assertion failed: expected 4", "model"),
      ent("rule", "Concept", "rounding", "user"),
    ],
    facts: [
      fact("f1", "file", "defines", { object_id: "avg" }),
      fact("f2", "file", "defines", { object_id: "add" }),
      fact("f3", "cmd", "failed_with", { object_id: "err" }),
      fact("f4", "avg", "has_bug", { text: "divides by n+1", inject: true, source: "user" }),
      fact("f5", "goal", "task", { text: "Run the tests and fix the failing one" }),
      fact("f6", "avg", "depends_on", { object_id: "rule", inject: true, source: "user" }),
    ],
    memory: null,
    edited_nodes: [],
  };
}

describe("labels", () => {
  it("summarizes facts in one line", () => {
    const k = sample();
    const m = byId(k);
    expect(factSummary(k.facts[0], m)).toBe("src/math.js defines average");
    expect(factSummary(k.facts[3], m)).toBe("average: divides by n+1");
    expect(factSummary(fact("x", "avg", "is_pure"), m)).toBe("average (is_pure)");
    expect(factSummary(fact("y", "avg", "uses", { object_id: "add", text: "twice" }), m)).toBe("average uses add: twice");
    expect(factSummary(fact("z", "gone", "x"), m)).toBe("? (x)");
  });

  it("shortens long names, keeping the end of a path", () => {
    expect(shortName("short")).toBe("short");
    expect(shortName("  a   b  ")).toBe("a b");
    expect(shortName("x".repeat(40))).toBe("x".repeat(27) + "…");
    const p = shortName("crates/loom-core/src/some/very/long/folder/policy.rs");
    expect(p.startsWith("…") && p.endsWith("policy.rs") && p.length === 28).toBe(true);
  });

  it("estimates tokens like the rest of Loom", () => {
    expect([estimateTokens(""), estimateTokens("abcd"), estimateTokens("abcde")]).toEqual([0, 1, 2]);
  });
});

describe("elements", () => {
  it("entities become nodes and linking facts become labelled edges", () => {
    const els = buildElements(sample(), NO_FILTER);
    const nodes = els.filter((e) => e.group === "nodes");
    const edges = els.filter((e) => e.group === "edges");
    expect(nodes).toHaveLength(7);
    expect(edges.map((e) => e.data.id).sort()).toEqual(["f1", "f2", "f3", "f6"]);
    const f1 = edges.find((e) => e.data.id === "f1")!.data as { source: string; target: string; label: string };
    expect([f1.source, f1.target, f1.label]).toEqual(["file", "avg", "defines"]);
  });

  it("facts about one thing become notes on its node, and sent facts are flagged", () => {
    const els = buildElements(sample(), NO_FILTER);
    const node = (id: string) => els.find((e) => e.group === "nodes" && e.data.id === id)!.data as { notes: number; sent: boolean; size: number; color: string };
    expect(node("avg").notes).toBe(1); // has_bug
    expect(node("avg").sent).toBe(true);
    expect(node("rule").sent).toBe(true); // the object of a sent edge
    expect(node("file").sent).toBe(false);
    expect(node("goal").notes).toBe(1);
    expect(node("avg").size).toBeGreaterThan(node("cmd").size - 1);
    expect(node("file").color).toBe("#34d399");
    const sentEdge = els.find((e) => e.group === "edges" && e.data.id === "f6")!.data as { sent: boolean; origin: string };
    expect([sentEdge.sent, sentEdge.origin]).toEqual([true, "user"]);
  });

  it("hiding a kind removes those entities and every edge that touches them", () => {
    const f = { ...NO_FILTER, hiddenKinds: new Set(["Symbol" as const]) };
    const els = buildElements(sample(), f);
    expect(els.filter((e) => e.group === "nodes")).toHaveLength(5);
    expect(els.filter((e) => e.group === "edges").map((e) => e.data.id)).toEqual(["f3"]);
  });

  it("search matches names, notes and the facts about an entity", () => {
    const k = sample();
    const names = (q: string) => visibleEntities(k, { ...NO_FILTER, query: q }).map((e) => e.id);
    expect(names("MATH")).toEqual(["file"]);
    // matches the text of a fact about it
    expect(names("divides")).toEqual(["avg"]);
    // matches the relation name
    expect(names("has_bug")).toEqual(["avg"]);
    expect(names("nothing like this")).toEqual([]);
    k.entities[1].note = "the main module";
    expect(names("main module")).toEqual(["file"]);
  });

  it("'only what is sent' keeps entities that take part in a marked fact", () => {
    const ids = visibleEntities(sample(), { ...NO_FILTER, onlySent: true }).map((e) => e.id).sort();
    expect(ids).toEqual(["avg", "rule"]);
  });
});

describe("lists", () => {
  it("groups entities by kind in a fixed order, skipping empty kinds", () => {
    const g = groupByKind(sample().entities);
    expect(g.map(([kind]) => kind)).toEqual(["File", "Symbol", "Command", "Error", "Goal", "Concept"]);
    expect(g[1][1].map((e) => e.name)).toEqual(["average", "add"]);
  });

  it("finds the facts that touch an entity, as subject or object", () => {
    const k = sample();
    expect(factsAbout(k, "avg").map((f) => f.id).sort()).toEqual(["f1", "f4", "f6"]);
    expect(factsAbout(k, "nobody")).toEqual([]);
  });

  it("counts what is yours and what is sent", () => {
    expect(counts(sample())).toEqual({ entities: 7, facts: 6, sent: 2, mine: 3 });
  });
});
