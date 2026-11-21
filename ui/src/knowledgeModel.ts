// Pure helpers for the knowledge graph view: how entities and facts become graph elements, lists and labels.

import type { Entity, EntityKind, Fact, Knowledge } from "./api";

export const KIND_META: Record<EntityKind, { color: string; shape: string; blurb: string }> = {
  File: { color: "#34d399", shape: "round-rectangle", blurb: "A file the agent read or changed." },
  Symbol: { color: "#a78bfa", shape: "ellipse", blurb: "A function, class or type defined in a file." },
  Command: { color: "#f472b6", shape: "rectangle", blurb: "A command the agent ran." },
  Error: { color: "#f87171", shape: "diamond", blurb: "An error or failure that came up." },
  Goal: { color: "#2dd4bf", shape: "star", blurb: "What the agent was asked to do." },
  Concept: { color: "#60a5fa", shape: "hexagon", blurb: "An idea or rule you added." },
  Note: { color: "#facc15", shape: "round-tag", blurb: "A free note you added." },
};

export const KINDS = Object.keys(KIND_META) as EntityKind[];

/** Common relations, offered as suggestions (any text is allowed). */
export const PREDICATES = ["defines", "edited", "tests", "depends_on", "caused_by", "fixed_by", "failed_with", "result", "is", "has_bug"];

export const SOURCE_LABEL = { auto: "extracted", model: "model", user: "you" } as const;

export function shortName(name: string, max = 28): string {
  const t = name.replace(/\s+/g, " ").trim();
  if (t.length <= max) return t;
  // for paths keep the end, which is the interesting part
  return /[\\/]/.test(t) ? "…" + t.slice(-(max - 1)) : t.slice(0, max - 1).trimEnd() + "…";
}

export function byId(k: Knowledge): Map<string, Entity> {
  return new Map(k.entities.map((e) => [e.id, e]));
}

/** "src/math.js defines average", or "average: returns 0" for a note about one thing. */
export function factSummary(f: Fact, ents: Map<string, Entity>): string {
  const s = ents.get(f.subject_id)?.name ?? "?";
  const o = f.object_id ? (ents.get(f.object_id)?.name ?? "?") : null;
  if (o) return `${s} ${f.predicate} ${o}${f.text ? `: ${f.text}` : ""}`;
  return f.text ? `${s}: ${f.text}` : `${s} (${f.predicate})`;
}

export type Filter = { query: string; hiddenKinds: Set<EntityKind>; onlySent: boolean };

export const NO_FILTER: Filter = { query: "", hiddenKinds: new Set(), onlySent: false };

/** Entities that pass the filter. With "only what is sent", just those that take part in a marked fact. */
export function visibleEntities(k: Knowledge, f: Filter): Entity[] {
  const q = f.query.trim().toLowerCase();
  const sent = new Set<string>();
  if (f.onlySent) {
    for (const x of k.facts) {
      if (x.inject) {
        sent.add(x.subject_id);
        if (x.object_id) sent.add(x.object_id);
      }
    }
  }
  const factText = new Map<string, string>();
  if (q) for (const x of k.facts) factText.set(x.subject_id, `${factText.get(x.subject_id) ?? ""} ${x.predicate} ${x.text}`.toLowerCase());
  return k.entities.filter(
    (e) =>
      !f.hiddenKinds.has(e.kind) &&
      (!f.onlySent || sent.has(e.id)) &&
      (!q || e.name.toLowerCase().includes(q) || (e.note ?? "").toLowerCase().includes(q) || (factText.get(e.id) ?? "").includes(q)),
  );
}

export type GraphEl =
  | { group: "nodes"; data: { id: string; label: string; kind: EntityKind; color: string; shape: string; notes: number; sent: boolean; source: string; size: number } }
  | { group: "edges"; data: { id: string; source: string; target: string; label: string; sent: boolean; origin: string } };

/** Cytoscape elements: entities are nodes; facts that link two entities are labelled edges. Facts about one thing are
 *  notes on its node (counted, and flagged when sent). */
export function buildElements(k: Knowledge, f: Filter): GraphEl[] {
  const ents = visibleEntities(k, f);
  const ids = new Set(ents.map((e) => e.id));
  const degree = new Map<string, number>();
  const notes = new Map<string, number>();
  const sentNote = new Set<string>();
  for (const x of k.facts) {
    if (x.object_id) {
      for (const id of [x.subject_id, x.object_id]) degree.set(id, (degree.get(id) ?? 0) + 1);
    } else {
      notes.set(x.subject_id, (notes.get(x.subject_id) ?? 0) + 1);
      if (x.inject) sentNote.add(x.subject_id);
    }
  }
  const els: GraphEl[] = ents.map((e) => ({
    group: "nodes",
    data: {
      id: e.id,
      label: shortName(e.name),
      kind: e.kind,
      color: KIND_META[e.kind].color,
      shape: KIND_META[e.kind].shape,
      notes: notes.get(e.id) ?? 0,
      sent: sentNote.has(e.id) || k.facts.some((x) => x.inject && (x.subject_id === e.id || x.object_id === e.id)),
      source: e.source,
      size: 26 + Math.min(30, (degree.get(e.id) ?? 0) * 4 + (notes.get(e.id) ?? 0) * 2),
    },
  }));
  for (const x of k.facts) {
    if (x.object_id && ids.has(x.subject_id) && ids.has(x.object_id)) {
      els.push({ group: "edges", data: { id: x.id, source: x.subject_id, target: x.object_id, label: x.predicate, sent: x.inject, origin: x.source } });
    }
  }
  return els;
}

export function factsAbout(k: Knowledge, entityId: string): Fact[] {
  return k.facts.filter((f) => f.subject_id === entityId || f.object_id === entityId);
}

export function counts(k: Knowledge): { entities: number; facts: number; sent: number; mine: number } {
  return {
    entities: k.entities.length,
    facts: k.facts.length,
    sent: k.facts.filter((f) => f.inject).length,
    mine: k.entities.filter((e) => e.source === "user").length + k.facts.filter((f) => f.source === "user").length,
  };
}

/** Entities grouped by kind in a fixed order, for the list. */
export function groupByKind(list: Entity[]): [EntityKind, Entity[]][] {
  return KINDS.map((kind) => [kind, list.filter((e) => e.kind === kind)] as [EntityKind, Entity[]]).filter(([, es]) => es.length > 0);
}

/** Rough token estimate (about 4 characters per token), matching the rest of Loom. */
export function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}
