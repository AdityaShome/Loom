import { useEffect, useMemo, useRef, useState } from "react";
import cytoscape from "cytoscape";
import { api, type Edge, type EdgeKind, type NodeRow } from "./api";
import { useLoad } from "./hooks";
import { fmtTokens } from "./format";

type GroupBy = "turn" | "tool" | "none";
const COL_W = 150;
const ROW_H = 62;
const AUTO_COLLAPSE_ABOVE = 150;
const EDGE_KINDS: EdgeKind[] = ["ToolPair", "Supersedes", "RefersTo", "Follows"];
const EDGE_HELP: Record<string, string> = {
  ToolPair: "tool call ↔ result (must stay together)",
  Supersedes: "newer read replaces older",
  RefersTo: "mentions an earlier file",
  Follows: "turn order",
};

const css = (name: string) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();

/** Group key per node. For "tool", results join the tool call they answer. */
function groupKeys(nodes: NodeRow[], edges: Edge[], by: GroupBy): Map<string, string> {
  const out = new Map<string, string>();
  if (by === "turn") nodes.forEach((n) => out.set(n.id, `Turn ${n.first_seen_turn + 1}`));
  else if (by === "none") nodes.forEach((n, i) => out.set(n.id, `Nodes ${Math.floor(i / 20) * 20 + 1}+`));
  else {
    const byId = new Map(nodes.map((n) => [n.id, n]));
    const callOf = new Map(edges.filter((e) => e.kind === "ToolPair").map((e) => [e.to, e.from]));
    for (const n of nodes) {
      const call = byId.get(callOf.get(n.id) ?? "");
      out.set(n.id, call?.source ?? (n.kind === "ToolCall" ? (n.source ?? "ToolCall") : n.kind));
    }
  }
  return out;
}

export default function GraphView({ sessionId, tick, latest }: { sessionId: string; tick: number; latest: number }) {
  const { data, error } = useLoad(() => api.graph(sessionId), [sessionId, tick]);
  const [groupBy, setGroupBy] = useState<GroupBy>("turn");
  const [showEdges, setShowEdges] = useState<Record<string, boolean>>({ ToolPair: true, Supersedes: true, RefersTo: true, Follows: false });
  const [collapsed, setCollapsed] = useState<Set<string> | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  const box = useRef<HTMLDivElement>(null);
  const cy = useRef<cytoscape.Core | null>(null);
  const refit = useRef(true);

  const nodes = data?.nodes;
  const edges = data?.edges;
  const groups = useMemo(() => (nodes && edges ? groupKeys(nodes, edges, groupBy) : new Map<string, string>()), [nodes, edges, groupBy]);
  const byId = useMemo(() => new Map((nodes ?? []).map((n) => [n.id, n])), [nodes]);

  // Start large graphs mostly collapsed; reset whenever the grouping changes.
  useEffect(() => {
    setCollapsed(null);
    refit.current = true;
  }, [groupBy, sessionId]);
  useEffect(() => {
    if (collapsed !== null || !nodes) return;
    if (groupBy === "none" || nodes.length <= AUTO_COLLAPSE_ABOVE) return setCollapsed(new Set());
    const order = [...new Set(nodes.map((n) => groups.get(n.id)!))];
    setCollapsed(new Set(order.slice(0, Math.max(0, order.length - 3))));
  }, [collapsed, nodes, groups, groupBy]);

  // Create / destroy the Cytoscape instance.
  useEffect(() => {
    const c = cytoscape({ container: box.current, minZoom: 0.15, maxZoom: 2.5, wheelSensitivity: 0.3 });
    cy.current = c;
    c.on("tap", "node", (ev) => {
      const d = ev.target.data();
      if (d.kind_ === "group" || d.kind_ === "header") {
        setCollapsed((prev) => {
          const next = new Set(prev ?? []);
          if (!next.delete(d.key)) next.add(d.key);
          return next;
        });
      } else setSelected(d.id);
    });
    c.on("tap", (ev) => {
      if (ev.target === c) setSelected(null);
    });
    return () => {
      c.destroy();
      cy.current = null;
    };
  }, []);

  // Rebuild elements whenever data, grouping, collapsing, edge filters or selection change.
  useEffect(() => {
    const c = cy.current;
    if (!c || !nodes || !edges || collapsed === null) return;
    const collapsible = groupBy !== "none";

    // Column order = first appearance of each group.
    const order: string[] = [];
    const members = new Map<string, NodeRow[]>();
    for (const n of nodes) {
      const k = groups.get(n.id)!;
      if (!members.has(k)) {
        members.set(k, []);
        order.push(k);
      }
      members.get(k)!.push(n);
    }
    const maxTok = Math.max(1, ...nodes.map((n) => n.tokens));
    const maxGroup = Math.max(1, ...[...members.values()].map((m) => m.reduce((s, n) => s + n.tokens, 0)));
    const kindColor = (k: string) => css(`--k-${k}`) || "#888";

    const els: cytoscape.ElementDefinition[] = [];
    const shown = (id: string) => {
      const k = groups.get(id)!;
      return collapsible && collapsed.has(k) ? `g:${k}` : id;
    };

    order.forEach((k, col) => {
      const x = col * COL_W;
      const ms = members.get(k)!;
      const total = ms.reduce((s, n) => s + n.tokens, 0);
      if (collapsible && collapsed.has(k)) {
        els.push({
          data: { id: `g:${k}`, key: k, kind_: "group", label: `${k}\n${ms.length} nodes`, size: 30 + 26 * Math.sqrt(total / maxGroup) },
          position: { x, y: ROW_H },
        });
        return;
      }
      if (collapsible) {
        els.push({ data: { id: `h:${k}`, key: k, kind_: "header", label: `${k} ▾` }, position: { x, y: 0 } });
      }
      ms.forEach((n, i) =>
        els.push({
          data: {
            id: n.id,
            kind_: "node",
            label: n.source ? n.source.split(/[\\/]/).pop() : n.kind,
            size: 14 + 26 * Math.sqrt(n.tokens / maxTok),
            status: n.status,
            kc: kindColor(n.kind),
          },
          position: { x, y: ROW_H * (i + 1) },
        }),
      );
    });

    const seen = new Set<string>();
    for (const e of edges) {
      if (!showEdges[e.kind]) continue;
      const s = shown(e.from);
      const t = shown(e.to);
      if (s === t || !c) continue;
      const key = `${s}|${t}|${e.kind}`;
      if (seen.has(key)) continue;
      seen.add(key);
      els.push({ data: { id: `e:${key}`, source: s, target: t, kind: e.kind } });
    }

    const stylesheet: cytoscape.StylesheetJson = [
      {
        selector: "node",
        style: {
          width: "data(size)",
          height: "data(size)",
          "background-color": ((el: cytoscape.NodeSingular) => css(`--st-${el.data("status")}`) || "#888") as never,
          "border-width": 3,
          "border-color": "data(kc)",
          label: "data(label)",
          "font-size": 9,
          color: css("--muted") || "#777",
          "text-valign": "bottom",
          "text-margin-y": 3,
          "text-wrap": "ellipsis",
          "text-max-width": "120px",
        },
      },
      {
        selector: "node[kind_ = 'group']",
        style: { shape: "round-rectangle", "background-color": css("--line") || "#ddd", "border-color": css("--muted") || "#888", "text-wrap": "wrap", "text-valign": "center", "text-margin-y": 0, color: css("--text") || "#000", "font-size": 10 },
      },
      {
        selector: "node[kind_ = 'header']",
        style: { shape: "round-rectangle", width: 70, height: 20, "background-color": css("--line") || "#ddd", "border-width": 0, "text-valign": "center", "text-margin-y": 0, color: css("--text") || "#000", "font-size": 10 },
      },
      { selector: "node:selected", style: { "border-width": 5, "overlay-opacity": 0.12 } },
      { selector: "edge", style: { width: 1.5, "curve-style": "bezier", "target-arrow-shape": "triangle", "arrow-scale": 0.8, opacity: 0.8 } },
      { selector: "edge[kind = 'ToolPair']", style: { "line-color": css("--e-ToolPair"), "target-arrow-color": css("--e-ToolPair"), width: 2.5 } },
      { selector: "edge[kind = 'Supersedes']", style: { "line-color": css("--e-Supersedes"), "target-arrow-color": css("--e-Supersedes"), "line-style": "dashed" } },
      { selector: "edge[kind = 'RefersTo']", style: { "line-color": css("--e-RefersTo"), "target-arrow-color": css("--e-RefersTo"), "line-style": "dotted", width: 2 } },
      { selector: "edge[kind = 'Follows']", style: { "line-color": css("--e-Follows"), "target-arrow-color": css("--e-Follows"), width: 1, opacity: 0.5 } },
      { selector: "edge.hl", style: { width: 4, opacity: 1 } },
    ];

    c.batch(() => {
      c.elements().remove();
      c.add(els);
      c.style(stylesheet);
      if (selected) {
        const n = c.getElementById(selected);
        if (n.nonempty()) {
          n.select();
          n.connectedEdges().addClass("hl");
        }
      }
    });
    if (refit.current && els.length) {
      c.fit(undefined, 30);
      if (c.zoom() > 1) c.zoom(1), c.center();
      refit.current = false;
    }
  }, [nodes, edges, groups, groupBy, collapsed, showEdges, selected]);

  const rels = useMemo(() => {
    if (!selected || !edges) return [];
    return edges
      .filter((e) => e.from === selected || e.to === selected)
      .filter((e) => e.kind !== "Follows")
      .map((e) => ({ kind: e.kind, dir: e.from === selected ? "→" : "←", other: e.from === selected ? e.to : e.from }));
  }, [selected, edges]);

  const pick = (id: string) => {
    const k = groups.get(id);
    if (k && collapsed?.has(k)) setCollapsed((p) => (p ? new Set([...p].filter((x) => x !== k)) : p));
    setSelected(id);
  };

  // The Cytoscape container must stay mounted: the instance is created once, on mount.
  const sel = selected ? byId.get(selected) : undefined;
  const counts = EDGE_KINDS.map((k) => [k, (edges ?? []).filter((e) => e.kind === k).length] as const);
  const status = error ? (
    <p className="error">{error}</p>
  ) : !data ? (
    <p className="muted">Loading…</p>
  ) : data.nodes.length === 0 ? (
    <p className="muted">No nodes yet.</p>
  ) : null;

  return (
    <>
      {status}
      <div className="filters">
        <label>
          Group by{" "}
          <select value={groupBy} onChange={(e) => setGroupBy(e.target.value as GroupBy)}>
            <option value="turn">turn</option>
            <option value="tool">tool</option>
            <option value="none">none</option>
          </select>
        </label>
        {counts.map(([k, n]) => (
          <label key={k} title={EDGE_HELP[k]}>
            <input type="checkbox" checked={!!showEdges[k]} onChange={(e) => setShowEdges({ ...showEdges, [k]: e.target.checked })} />{" "}
            <span className={`ekind ${k}`}>{k}</span> <span className="muted">{n}</span>
          </label>
        ))}
        <span className="muted">click a column header to collapse or expand it</span>
      </div>
      <div className="graph-wrap">
        <div>
          <div ref={box} className="cy" role="img" aria-label="Context graph" />
          <div className="glegend">
            <span>size = tokens</span>
            {(["Full", "Summarized", "Dropped", "DroppedByAgent"] as const).map((s) => (
              <span key={s}>
                <i className="sw" style={{ background: `var(--st-${s})` }} />
                {s === "DroppedByAgent" ? "dropped by agent" : s.toLowerCase()}
              </span>
            ))}
            <span>ring colour = kind</span>
            <span className="muted">newest turn: {latest + 1}</span>
          </div>
        </div>
        <aside className="side">
          {!sel ? (
            <p className="muted">Select a node to see what it connects to.</p>
          ) : (
            <>
              <h4>
                <span className={`badge k-${sel.kind}`}>{sel.kind}</span> {sel.source && <code>{sel.source}</code>}
              </h4>
              <div className="kv">
                <span>{fmtTokens(sel.tokens)} tokens</span>
                <span>
                  t{sel.first_seen_turn + 1}–t{sel.last_seen_turn + 1}
                </span>
                <span>{sel.status === "DroppedByAgent" ? "dropped by agent" : sel.status.toLowerCase()}</span>
              </div>
              <pre>{sel.preview || "(empty)"}</pre>
              <strong>Relations</strong>
              {rels.length === 0 ? (
                <p className="muted">None besides turn order.</p>
              ) : (
                <ul className="rels">
                  {rels.map((r, i) => {
                    const o = byId.get(r.other);
                    return (
                      <li key={i} onClick={() => pick(r.other)}>
                        <span className={`ekind ${r.kind}`}>
                          {r.dir} {r.kind}
                        </span>
                        <span className={`badge k-${o?.kind}`}>{o?.kind ?? "?"}</span>
                        <span className="preview">{o?.source ?? o?.preview ?? r.other}</span>
                      </li>
                    );
                  })}
                </ul>
              )}
            </>
          )}
        </aside>
      </div>
    </>
  );
}
