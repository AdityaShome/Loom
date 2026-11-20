import { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
import { api, type PlanResponse, type Session, type Turn } from "./api";
import { fmtTokens } from "./format";
import { useLoad } from "./hooks";
import { Icon } from "./icons";
import ContextFlow from "./ContextFlow";
import GraphPanel from "./GraphPanel";
import MindMap from "./MindMap";
import {
  basename,
  buildModel,
  decisionOf,
  DECISION_COLOR,
  HUB_META,
  hubOf,
  shorten,
  type Ctx,
  type HubId,
  type Model,
  type Selection,
} from "./graphModel";
import "./contextgraph.css";

// The turn-by-turn column graph (Cytoscape) is large; load it only when asked for.
const Timeline = lazy(() => import("./GraphView"));

type Sub = "graph" | "layers" | "timeline";
const SUBS: [Sub, string, "graph" | "layers" | "clock"][] = [
  ["graph", "Graph View", "graph"],
  ["layers", "Context Layers", "layers"],
  ["timeline", "Timeline", "clock"],
];

function Layers({ model, ctx, onPick }: { model: Model; ctx: Ctx; onPick: (s: Selection) => void }) {
  const hubs = model.hubs.filter((h) => HUB_META[h.id].content);
  const over = ctx.budget > 0 && model.total > ctx.budget;
  const heavy = [...model.nodes].sort((a, b) => b.tokens - a.tokens).slice(0, 8);
  return (
    <div className="cg-layers">
      <div className="cg-stack" role="img" aria-label="Tokens by layer">
        {hubs.map((h) => (
          <span key={h.id} style={{ width: `${(h.tokens / Math.max(model.total, ctx.budget, 1)) * 100}%`, background: h.color }} title={`${h.title}: ${fmtTokens(h.tokens)}`} />
        ))}
        {ctx.budget > 0 && <i className="cg-budgetmark" style={{ left: `${Math.min(100, (ctx.budget / Math.max(model.total, ctx.budget)) * 100)}%` }} title={`Budget ${fmtTokens(ctx.budget)}`} />}
      </div>
      <p className="cg-note">
        {fmtTokens(model.total)} tokens{ctx.budget ? ` of a ${fmtTokens(ctx.budget)} budget` : ""}
        {over ? " (over budget)" : ""}
      </p>
      <table className="cg-table">
        <thead>
          <tr>
            <th>Layer</th>
            <th className="num">Nodes</th>
            <th className="num">Tokens</th>
            <th>Share</th>
          </tr>
        </thead>
        <tbody>
          {hubs.map((h) => (
            <tr key={h.id} onClick={() => onPick({ kind: "hub", id: h.id })} tabIndex={0} onKeyDown={(e) => e.key === "Enter" && onPick({ kind: "hub", id: h.id })}>
              <td>
                <i className="cg-dot" style={{ background: h.color }} /> {h.title}
              </td>
              <td className="num">{h.nodeIds.length}</td>
              <td className="num">{fmtTokens(h.tokens)}</td>
              <td>
                <div className="cg-bar">
                  <b style={{ width: `${model.total ? (h.tokens / model.total) * 100 : 0}%`, background: h.color }} />
                </div>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <h4>Heaviest nodes</h4>
      <ul className="cg-heavy">
        {heavy.map((n) => {
          const d = decisionOf(n, model.planById);
          return (
            <li key={n.id} onClick={() => onPick({ kind: "node", id: n.id })}>
              <i className="cg-dot" style={{ background: HUB_META[hubOf(n)].color }} />
              <span>{n.source ? basename(n.source) : shorten(n.preview || n.kind, 40)}</span>
              <em style={{ color: DECISION_COLOR[d] }}>{d}</em>
              <b>{fmtTokens(n.tokens)}</b>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

export default function ContextGraph({
  sessionId,
  session,
  plan,
  turns,
  latest,
  tick,
  connected,
}: {
  sessionId: string;
  session: Session | undefined;
  plan: PlanResponse | null;
  turns: Turn[];
  latest: number;
  tick: number;
  connected: boolean;
}) {
  const graph = useLoad(() => api.graph(sessionId), [sessionId, tick]);
  const rules = useLoad(() => api.rules(sessionId), [sessionId, tick]);
  const [sub, setSub] = useState<Sub>("graph");
  const [selection, setSelection] = useState<Selection>({ kind: "center" });
  const [query, setQuery] = useState("");
  const [hidden, setHidden] = useState<Set<HubId>>(new Set());
  const [fitSignal, setFitSignal] = useState(0);
  const [full, setFull] = useState(false);
  const root = useRef<HTMLElement>(null);

  useEffect(() => {
    const on = () => setFull(document.fullscreenElement === root.current);
    document.addEventListener("fullscreenchange", on);
    return () => document.removeEventListener("fullscreenchange", on);
  }, []);

  const mode = plan?.mode ?? session?.mode ?? "Observe";
  const model = useMemo(
    () =>
      buildModel({
        nodes: graph.data?.nodes ?? [],
        edges: graph.data?.edges ?? [],
        plan: plan?.plan ?? null,
        mode,
        latest,
        rules: rules.data ?? [],
        budget: plan?.budget_tokens,
      }),
    [graph.data, rules.data, plan, mode, latest],
  );
  const ctx: Ctx = { budget: plan?.budget_tokens ?? 0, plan: plan?.plan ?? null, mode, latest };
  const title = session?.name ?? sessionId;
  const subtitle = `${session?.model ?? "model"} · turn ${latest + 1}`;

  // A selection that no longer exists (a node left the context) falls back to the centre.
  useEffect(() => {
    if (selection.kind === "node" && !model.byId.has(selection.id)) setSelection({ kind: "center" });
    if (selection.kind === "hub" && !model.hubs.some((h) => h.id === selection.id)) setSelection({ kind: "center" });
    if (selection.kind === "leaf") {
      const h = model.hubs.find((x) => x.id === selection.hub);
      if (!h?.leaves.some((l) => l.id === selection.leaf)) setSelection({ kind: "center" });
    }
  }, [model, selection]);

  const pick = (s: Selection) => {
    setSelection(s);
    if (s.kind === "node" || s.kind === "hub") setSub("graph");
  };

  return (
    <section className={`cg ${full ? "full" : ""}`} ref={root} aria-label="Context graph">
      <header className="cg-head">
        <div>
          <h3>Context Graph</h3>
          <p>What the model sees on the next call, grouped by where it came from</p>
        </div>
        <span className={`cg-live ${connected ? "on" : ""}`}>{connected ? "Live" : "Reconnecting"}</span>
      </header>

      <div className="cg-bar-row">
        <nav className="cg-tabs" role="tablist">
          {SUBS.map(([k, label, icon]) => (
            <button key={k} role="tab" aria-selected={sub === k} className={sub === k ? "active" : ""} onClick={() => setSub(k)}>
              <Icon name={icon} size={16} /> {label}
            </button>
          ))}
        </nav>
        {sub === "graph" && (
          <div className="cg-tools">
            <label className="cg-search">
              <Icon name="search" size={16} />
              <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search nodes…" aria-label="Search nodes" />
            </label>
            <details className="cg-filter">
              <summary aria-label="Filter categories" title="Show or hide categories">
                <Icon name="filter" size={17} />
              </summary>
              <div className="cg-pop">
                {model.hubs.map((h) => (
                  <label key={h.id}>
                    <input
                      type="checkbox"
                      checked={!hidden.has(h.id)}
                      onChange={() => setHidden((p) => (p.has(h.id) ? new Set([...p].filter((x) => x !== h.id)) : new Set([...p, h.id])))}
                    />
                    <i className="cg-dot" style={{ background: h.color }} /> {h.title}
                  </label>
                ))}
              </div>
            </details>
            <button className="cg-iconbtn" onClick={() => setFitSignal((n) => n + 1)} aria-label="Fit to view" title="Fit to view">
              <Icon name="fit" size={17} />
            </button>
            <button
              className="cg-iconbtn"
              onClick={() => (document.fullscreenElement ? document.exitFullscreen() : root.current?.requestFullscreen())}
              aria-label="Toggle fullscreen"
              title="Fullscreen"
            >
              <Icon name="expand" size={17} />
            </button>
          </div>
        )}
      </div>

      {graph.error && <p className="error">{graph.error}</p>}

      {sub === "graph" && (
        <>
          <div className="cg-body">
            <MindMap
              model={model}
              hidden={hidden}
              selection={selection}
              onSelect={pick}
              title={title}
              subtitle={subtitle}
              query={query}
              fitSignal={fitSignal}
            />
            <GraphPanel selection={selection} model={model} ctx={ctx} title={title} subtitle={subtitle} onSelect={pick} />
          </div>
          <ContextFlow model={model} ctx={ctx} turns={turns} />
        </>
      )}
      {sub === "layers" && (
        <div className="cg-body one">
          <Layers model={model} ctx={ctx} onPick={pick} />
        </div>
      )}
      {sub === "timeline" && (
        <div className="cg-light">
          <Suspense fallback={<p className="muted">Loading…</p>}>
            <Timeline sessionId={sessionId} tick={tick} latest={latest} />
          </Suspense>
        </div>
      )}
    </section>
  );
}
