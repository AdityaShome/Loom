import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Icon } from "./icons";
import type { Hub, HubId, Leaf, Model, Selection } from "./graphModel";

// Design coordinates; the stage is scaled to fit the available space. The height grows with
// the number of blocks on a side, so any number of status blocks fit without overlapping.
export const W = 1100;
const CX = W / 2;
const CENTER = { w: 214, h: 84 };
const HUB = { w: 212, h: 66 };
const LEAF = { w: 140, h: 30 };
const HUB_X = { left: 285, right: 815 };
const LEAF_X = { left: 76, right: 1024 };
const ROW = 150;

type Pt = [number, number];

type Placed = { hub: Hub; x: number; y: number; left: boolean };

/** Place the visible hubs: evenly down each side, in order. */
export function layoutHubs(hubs: Hub[]): { placed: Placed[]; H: number; CY: number } {
  const side = (s: "left" | "right") => hubs.filter((h) => h.side === s).sort((a, b) => a.order - b.order);
  const left = side("left");
  const right = side("right");
  const rows = Math.max(left.length, right.length, 3);
  const H = Math.max(620, rows * ROW + 36);
  const place = (list: Hub[], isLeft: boolean): Placed[] =>
    list.map((hub, i) => ({ hub, left: isLeft, x: isLeft ? HUB_X.left : HUB_X.right, y: (H / list.length) * (i + 0.5) }));
  return { placed: [...place(left, true), ...place(right, false)], H, CY: H / 2 };
}

/** Horizontal S-curve between two points. */
const curve = ([x1, y1]: Pt, [x2, y2]: Pt) => {
  const mx = (x1 + x2) / 2;
  return `M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}`;
};

/**
 * From the centre card to a hub. Hubs level with the centre leave from its side; the others leave from its
 * top or bottom edge. The farthest hub leaves nearest the axis so curves on one side never cross.
 */
function centerCurve(p: Placed, all: Placed[], CY: number): { d: string; from: Pt; to: Pt } {
  const to: Pt = [p.left ? p.x + HUB.w / 2 : p.x - HUB.w / 2, p.y];
  const dy = p.y - CY;
  if (Math.abs(dy) < 24) {
    const from: Pt = [p.left ? CX - CENTER.w / 2 : CX + CENTER.w / 2, CY];
    return { d: curve(from, to), from, to };
  }
  const sameHalf = all.filter((o) => o.left === p.left && Math.sign(o.y - CY) === Math.sign(dy) && Math.abs(o.y - CY) >= 24);
  const farther = sameHalf.filter((o) => Math.abs(o.y - CY) > Math.abs(dy)).length; // 0 = the farthest
  const offset = 26 + 24 * farther;
  const from: Pt = [CX + (p.left ? -offset : offset), dy < 0 ? CY - CENTER.h / 2 : CY + CENTER.h / 2];
  const d = `M${from[0]},${from[1]} C${from[0]},${to[1]} ${from[0] + (to[0] - from[0]) * 0.55},${to[1]} ${to[0]},${to[1]}`;
  return { d, from, to };
}

function leafY(i: number, n: number, hubY: number): number {
  return hubY + (i - (n - 1) / 2) * 46;
}

type Props = {
  model: Model;
  hidden: Set<HubId>;
  selection: Selection;
  onSelect: (s: Selection) => void;
  title: string;
  subtitle: string;
  query: string;
  fitSignal: number;
};

export default function MindMap({ model, hidden, selection, onSelect, title, subtitle, query, fitSignal }: Props) {
  const vp = useRef<HTMLDivElement>(null);
  const touched = useRef(false);
  const [view, setView] = useState({ k: 1, x: 0, y: 0 });

  const hubs = useMemo(() => model.hubs.filter((h) => !hidden.has(h.id)), [model.hubs, hidden]);
  const { placed, H, CY } = useMemo(() => layoutHubs(hubs), [hubs]);

  const fit = useCallback(() => {
    const el = vp.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    if (!width || !height) return;
    const k = Math.min(width / W, height / H, 1.2) * 0.98;
    setView({ k, x: (width - W * k) / 2, y: (height - H * k) / 2 });
  }, [H]);

  // Refit on request, when the number of blocks changes the height, and while the user has not moved the view.
  useEffect(() => {
    touched.current = false;
    fit();
  }, [fitSignal, fit]);
  useEffect(() => {
    const el = vp.current;
    if (!el) return;
    const ro = new ResizeObserver(() => !touched.current && fit());
    ro.observe(el);
    return () => ro.disconnect();
  }, [fit]);

  // Wheel zooms around the pointer (needs a non-passive listener to stop the page scrolling).
  useEffect(() => {
    const el = vp.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      touched.current = true;
      const r = el.getBoundingClientRect();
      const px = e.clientX - r.left;
      const py = e.clientY - r.top;
      setView((p) => {
        const k = Math.min(2.5, Math.max(0.3, p.k * Math.exp(-e.deltaY * 0.0015)));
        const ratio = k / p.k;
        return { k, x: px - (px - p.x) * ratio, y: py - (py - p.y) * ratio };
      });
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  const drag = useRef<{ x: number; y: number } | null>(null);
  const zoomBy = (f: number) => {
    touched.current = true;
    const el = vp.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    setView((p) => {
      const k = Math.min(2.5, Math.max(0.3, p.k * f));
      const ratio = k / p.k;
      return { k, x: width / 2 - (width / 2 - p.x) * ratio, y: height / 2 - (height / 2 - p.y) * ratio };
    });
  };

  // Briefly flash any block whose numbers just changed, so a live update is visible.
  const lastSeen = useRef<Map<string, string>>(new Map());
  const [flash, setFlash] = useState<Set<string>>(new Set());
  const signature = JSON.stringify([title, subtitle, hubs.map((h) => [h.id, h.subtitle, h.leaves.map((l) => l.sub)])]);
  useEffect(() => {
    const next = new Map<string, string>([["center", `${title}|${subtitle}|${model.total}`]]);
    hubs.forEach((h) => next.set(h.id, `${h.subtitle}|${h.leaves.map((l) => `${l.label}${l.sub}`).join(",")}`));
    const changed = new Set<string>();
    next.forEach((v, k) => lastSeen.current.has(k) && lastSeen.current.get(k) !== v && changed.add(k));
    lastSeen.current = next;
    if (!changed.size) return;
    setFlash(changed);
    const t = window.setTimeout(() => setFlash(new Set()), 1600);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [signature]);

  const q = query.trim().toLowerCase();
  const matching = useMemo(() => {
    if (!q) return null;
    return new Set(
      model.nodes.filter((n) => `${n.kind} ${n.source ?? ""} ${n.preview}`.toLowerCase().includes(q)).map((n) => n.id),
    );
  }, [model.nodes, q]);
  const dim = (ids: string[]) => (matching ? !ids.some((id) => matching.has(id)) : false);

  const isSel = (s: Selection) => JSON.stringify(s) === JSON.stringify(selection);
  const hubActive = (h: Hub) => (selection.kind === "hub" && selection.id === h.id) || (selection.kind === "leaf" && selection.hub === h.id);

  const links: { key: string; d: string; color: string; from: Pt; to: Pt; on: boolean; dim: boolean }[] = [];
  const cards: React.ReactNode[] = [];

  for (const p of placed) {
    const { hub: h, left } = p;
    const c = centerCurve(p, placed, CY);
    const hubDim = dim(h.nodeIds);
    links.push({ key: `c-${h.id}`, ...c, color: h.color, on: hubActive(h) || selection.kind === "center", dim: hubDim });

    cards.push(
      <button
        key={`hub-${h.id}`}
        className={`cg-card cg-hub ${isSel({ kind: "hub", id: h.id }) ? "sel" : ""} ${hubDim ? "dim" : ""} ${flash.has(h.id) ? "flash" : ""}`}
        style={{ left: p.x - HUB.w / 2, top: p.y - HUB.h / 2, width: HUB.w, height: HUB.h, ["--c" as string]: h.color }}
        onClick={() => onSelect({ kind: "hub", id: h.id })}
        aria-label={`${h.title}: ${h.subtitle}`}
      >
        <span className="cg-ico">
          <Icon name={h.icon} size={22} color={h.color} />
        </span>
        <span className="cg-txt">
          <b>{h.title}</b>
          <small>{h.subtitle}</small>
        </span>
      </button>,
    );

    const n = h.leaves.length;
    h.leaves.forEach((l: Leaf, i) => {
      const ly = leafY(i, n, p.y);
      const lx = left ? LEAF_X.left : LEAF_X.right;
      const from: Pt = [left ? p.x - HUB.w / 2 : p.x + HUB.w / 2, p.y];
      const to: Pt = [left ? lx + LEAF.w / 2 : lx - LEAF.w / 2, ly];
      const sel: Selection = l.more ? { kind: "hub", id: h.id } : { kind: "leaf", hub: h.id, leaf: l.id };
      const leafDim = l.nodeIds.length ? dim(l.nodeIds) : matching != null;
      links.push({
        key: `l-${h.id}-${l.id}`,
        d: curve(from, to),
        color: l.color,
        from,
        to,
        on: isSel(sel) || (hubActive(h) && selection.kind === "hub"),
        dim: leafDim,
      });
      cards.push(
        <button
          key={`leaf-${h.id}-${l.id}`}
          className={`cg-card cg-leaf ${isSel(sel) && !l.more ? "sel" : ""} ${leafDim ? "dim" : ""}`}
          style={{ left: lx - LEAF.w / 2, top: ly - LEAF.h / 2, width: LEAF.w, height: LEAF.h, ["--c" as string]: l.color }}
          onClick={() => onSelect(sel)}
          title={l.sub ? `${l.label} · ${l.sub}` : l.label}
          aria-label={l.sub ? `${l.label}, ${l.sub}` : l.label}
        >
          <i className="cg-dot" />
          <span>{l.label}</span>
        </button>,
      );
    });
  }

  const g = model.gauge;
  const gaugeColor = g.tone === "bad" ? "#f87171" : g.tone === "warn" ? "#fbbf24" : "#34d399";

  return (
    <div
      className="cg-viewport"
      ref={vp}
      onPointerDown={(e) => {
        if ((e.target as HTMLElement).closest("button")) return;
        drag.current = { x: e.clientX, y: e.clientY };
        e.currentTarget.setPointerCapture(e.pointerId);
        touched.current = true;
      }}
      onPointerMove={(e) => {
        if (!drag.current) return;
        const dx = e.clientX - drag.current.x;
        const dy = e.clientY - drag.current.y;
        drag.current = { x: e.clientX, y: e.clientY };
        setView((p) => ({ ...p, x: p.x + dx, y: p.y + dy }));
      }}
      onPointerUp={() => (drag.current = null)}
      onPointerCancel={() => (drag.current = null)}
      onClick={(e) => {
        if (e.target === e.currentTarget || (e.target as HTMLElement).classList.contains("cg-stage")) onSelect({ kind: "center" });
      }}
    >
      <div className="cg-stage" style={{ width: W, height: H, transform: `translate(${view.x}px, ${view.y}px) scale(${view.k})` }}>
        <svg className="cg-links" width={W} height={H} aria-hidden="true">
          {links.map((l) => (
            <g key={l.key} className={`${l.on ? "on" : ""} ${l.dim ? "dim" : ""}`} style={{ color: l.color }}>
              <path d={l.d} className="cg-path" />
              <circle cx={l.from[0]} cy={l.from[1]} r={3.2} fill="currentColor" />
              <circle cx={l.to[0]} cy={l.to[1]} r={3.2} fill="currentColor" />
            </g>
          ))}
        </svg>

        {model.nodes.length > 0 && (
          <button
            className={`cg-card cg-center ${selection.kind === "center" ? "sel" : ""} ${flash.has("center") ? "flash" : ""}`}
            style={{ left: CX - CENTER.w / 2, top: CY - CENTER.h / 2, width: CENTER.w, height: CENTER.h, ["--c" as string]: "#60a5fa" }}
            onClick={() => onSelect({ kind: "center" })}
            aria-label={`${title}: ${subtitle}`}
          >
            <span className="cg-ico big">
              <Icon name="cpu" size={30} color="#60a5fa" />
            </span>
            <span className="cg-txt">
              <b>{title}</b>
              <small>{subtitle}</small>
            </span>
            {g.budget > 0 && (
              <span
                className="cg-gauge"
                role="img"
                aria-label={`${Math.round(g.ratio * 100)}% of the token budget`}
                title={`${g.used.toLocaleString()} of ${g.budget.toLocaleString()} tokens`}
              >
                <i style={{ width: `${Math.min(100, g.ratio * 100)}%`, background: gaugeColor, boxShadow: `0 0 8px ${gaugeColor}` }} />
              </span>
            )}
          </button>
        )}
        {cards}
      </div>

      {model.nodes.length === 0 && <div className="cg-empty">No nodes in the latest request yet.</div>}

      <div className="cg-zoom">
        <button onClick={() => zoomBy(1.2)} aria-label="Zoom in">
          +
        </button>
        <button onClick={() => zoomBy(1 / 1.2)} aria-label="Zoom out">
          −
        </button>
        <button
          onClick={() => {
            touched.current = false;
            fit();
          }}
          aria-label="Fit to view"
        >
          <Icon name="fit" size={15} />
        </button>
      </div>
    </div>
  );
}
