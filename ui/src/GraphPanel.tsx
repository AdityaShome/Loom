import { useState } from "react";
import { fmtTokens } from "./format";
import { Icon } from "./icons";
import {
  centerInsights,
  HUB_META,
  historyOf,
  hubInsights,
  hubOf,
  nodeInsights,
  nodesOf,
  related,
  relationRows,
  shorten,
  basename,
  type Ctx,
  type IconName,
  type Insight,
  type Model,
  type Selection,
} from "./graphModel";

type Tab = "details" | "relations" | "history";

function describe(sel: Selection, m: Model, title: string, subtitle: string) {
  switch (sel.kind) {
    case "center":
      return {
        icon: "cpu" as IconName,
        color: "#60a5fa",
        title,
        subtitle: "Central node",
        blurb: "Everything the model receives on the next call: instructions, your requests, its own replies, tool results and files. Pick a category to look closer.",
      };
    case "hub": {
      const h = m.hubs.find((x) => x.id === sel.id);
      const meta = HUB_META[sel.id];
      return { icon: meta.icon, color: meta.color, title: meta.title, subtitle: h?.subtitle ?? subtitle, blurb: meta.blurb };
    }
    case "leaf": {
      const h = m.hubs.find((x) => x.id === sel.hub);
      const l = h?.leaves.find((x) => x.id === sel.leaf);
      return { icon: HUB_META[sel.hub].icon, color: l?.color ?? HUB_META[sel.hub].color, title: l?.label ?? "Group", subtitle: `${HUB_META[sel.hub].title} · ${l?.sub ?? ""}`, blurb: HUB_META[sel.hub].blurb };
    }
    case "node": {
      const n = m.byId.get(sel.id);
      const hub = n ? hubOf(n) : "knowledge";
      return {
        icon: HUB_META[hub].icon,
        color: HUB_META[hub].color,
        title: n ? (n.source ? basename(n.source) : shorten(n.preview || n.kind, 28)) : "Node",
        subtitle: n ? `${n.kind} · ${fmtTokens(n.tokens)} tokens` : "",
        blurb: n?.preview ? shorten(n.preview, 240) : "",
      };
    }
  }
}

function insightsFor(sel: Selection, m: Model, c: Ctx): Insight[] {
  switch (sel.kind) {
    case "center":
      return centerInsights(m, c);
    case "hub": {
      const h = m.hubs.find((x) => x.id === sel.id);
      return h ? hubInsights(h, m, c) : [];
    }
    case "leaf": {
      const ns = nodesOf(sel, m);
      const notes = m.hubs.find((h) => h.id === sel.hub)?.leaves.find((l) => l.id === sel.leaf)?.notes ?? [];
      if (notes.length) return notes.map((text) => ({ text }));
      return [
        { text: `${ns.length} node${ns.length === 1 ? "" : "s"}, ${fmtTokens(ns.reduce((s, n) => s + n.tokens, 0))} tokens` },
        ...ns.slice(0, 1).flatMap((n) => nodeInsights(n, m).slice(1, 3)),
      ];
    }
    case "node": {
      const n = m.byId.get(sel.id);
      return n ? nodeInsights(n, m) : [];
    }
  }
}

export default function GraphPanel({
  selection,
  model,
  ctx,
  title,
  subtitle,
  onSelect,
}: {
  selection: Selection;
  model: Model;
  ctx: Ctx;
  title: string;
  subtitle: string;
  onSelect: (s: Selection) => void;
}) {
  const [tab, setTab] = useState<Tab>("details");
  const d = describe(selection, model, title, subtitle);
  const insights = insightsFor(selection, model, ctx);
  const rel = related(selection, model);
  const ns = nodesOf(selection, model);
  const hist = historyOf(ns);
  const maxTok = Math.max(1, ...hist.map((h) => h.tokens));
  const rows = relationRows(selection, model);

  return (
    <aside className="cg-panel" aria-label="Node details">
      <header className="cg-ph">
        <span className="cg-ico big" style={{ ["--c" as string]: d.color }}>
          <Icon name={d.icon} size={28} color={d.color} />
        </span>
        <div>
          <h3>{d.title}</h3>
          <small>{d.subtitle}</small>
        </div>
      </header>

      <nav className="cg-ptabs" role="tablist">
        {(["details", "relations", "history"] as Tab[]).map((t) => (
          <button key={t} role="tab" aria-selected={tab === t} className={tab === t ? "active" : ""} onClick={() => setTab(t)}>
            {t[0].toUpperCase() + t.slice(1)}
          </button>
        ))}
      </nav>

      {tab === "details" && (
        <div className="cg-pbody">
          {d.blurb && <p className="cg-blurb">{d.blurb}</p>}
          <h4>
            <Icon name="bulb" size={16} color="#fbbf24" /> Key Insights
          </h4>
          <ul className="cg-insights">
            {insights.map((i, k) => (
              <li key={k} className={i.tone}>
                <Icon name={i.tone === "warn" || i.tone === "bad" ? "alert" : "check"} size={15} />
                <span>{i.text}</span>
              </li>
            ))}
          </ul>
          <h4>
            <Icon name="link" size={16} color="#60a5fa" /> Related Nodes ({rel.length})
          </h4>
          {rel.length === 0 ? (
            <p className="cg-none">Nothing related yet.</p>
          ) : (
            <ul className="cg-related">
              {rel.map((r) => (
                <li key={r.key}>
                  <button onClick={() => onSelect(r.target)}>
                    <Icon name={r.icon} size={16} color={r.color} />
                    <span>{r.label}</span>
                    <em>{r.value}</em>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {tab === "relations" && (
        <div className="cg-pbody">
          {rows.length === 0 ? (
            <p className="cg-none">No relations besides turn order. Tool pairs, references and superseded reads show up here.</p>
          ) : (
            <ul className="cg-rel">
              {rows.map((r, i) => (
                <li key={i}>
                  <i style={{ background: r.color }} />
                  {r.text}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {tab === "history" && (
        <div className="cg-pbody">
          <p className="cg-blurb">Tokens by the turn they first entered the context.</p>
          {hist.length === 0 ? (
            <p className="cg-none">No history.</p>
          ) : (
            <ul className="cg-hist">
              {hist.map((h) => (
                <li key={h.turn}>
                  <span>turn {h.turn + 1}</span>
                  <div>
                    <b style={{ width: `${(h.tokens / maxTok) * 100}%`, background: d.color }} />
                  </div>
                  <em>
                    {fmtTokens(h.tokens)} · {h.nodes}
                  </em>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </aside>
  );
}
