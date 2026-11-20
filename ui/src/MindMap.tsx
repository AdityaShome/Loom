import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Icon } from "./icons";
import type { Hub, HubId, Leaf, Model, Selection } from "./graphModel";

// Design coordinates; the stage is scaled to fit the available space.
export const W = 1100;
export const H = 620;
const CX = W / 2;
const CY = H / 2;
const CENTER = { w: 214, h: 78 };
const HUB = { w: 212, h: 66 };
const LEAF = { w: 140, h: 30 };
const HUB_X = { left: 285, right: 815 };
const LEAF_X = { left: 76, right: 1024 };
const SLOT_Y = [106, CY, H - 106];

type Pt = [number, number];

/** Horizontal S-curve between two points. */
const curve = ([x1, y1]: Pt, [x2, y2]: Pt) => {
  const mx = (x1 + x2) / 2;
  return `M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}`;
};

/** From the centre card: leave vertically for the top and bottom hubs, horizontally for the middle one. */
function centerCurve(h: Hub): { d: string; from: Pt; to: Pt } {
  const left = h.side === "left";
  const hy = SLOT_Y[h.slot];
  const to: Pt = [left ? HUB_X.left + HUB.w / 2 : HUB_X.right - HUB.w / 2, hy];
  if (h.slot === 1) {
    const from: Pt = [left ? CX - CENTER.w / 2 : CX + CENTER.w / 2, CY];
    return { d: curve(from, to), from, to };
  }
  const from: Pt = [CX + (left ? -52 : 52), h.slot === 0 ? CY - CENTER.h / 2 : CY + CENTER.h / 2];
  return { d: `M${from[0]},${from[1]} C${from[0]},${to[1]} ${from[0] + (to[0] - from[0]) * 0.55},${to[1]} ${to[0]},${to[1]}`, from, to };
}

function leafY(i: number, n: number, slot: number): number {
  const gap = 46;
  return SLOT_Y[slot] + (i - (n - 1) / 2) * gap;
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

  const fit = useCallback(() => {
    const el = vp.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    if (!width || !height) return;
    const k = Math.min(width / W, height / H, 1.2) * 0.98;
    setView({ k, x: (width - W * k) / 2, y: (height - H * k) / 2 });
  }, []);

  // Refit on request and while the user has not moved the view themselves.
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
        const k = Math.min(2.5, Math.max(0.35, p.k * Math.exp(-e.deltaY * 0.0015)));
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
      const k = Math.min(2.5, Math.max(0.35, p.k * f));
      const ratio = k / p.k;
      return { k, x: width / 2 - (width / 2 - p.x) * ratio, y: height / 2 - (height / 2 - p.y) * ratio };
    });
  };

  const q = query.trim().toLowerCase();
  const matching = useMemo(() => {
    if (!q) return null;
    return new Set(
      model.nodes
        .filter((n) => `${n.kind} ${n.source ?? ""} ${n.preview}`.toLowerCase().includes(q))
        .map((n) => n.id),
    );
  }, [model.nodes, q]);
  const dim = (ids: string[]) => (matching ? !ids.some((id) => matching.has(id)) : false);

  const hubs = model.hubs.filter((h) => !hidden.has(h.id));
  const isSel = (s: Selection) => JSON.stringify(s) === JSON.stringify(selection);
  const hubActive = (h: Hub) => selection.kind === "hub" && selection.id === h.id || selection.kind === "leaf" && selection.hub === h.id;

  const links: { key: string; d: string; color: string; from: Pt; to: Pt; on: boolean; dim: boolean }[] = [];
  const cards: React.ReactNode[] = [];

  for (const h of hubs) {
    const left = h.side === "left";
    const hy = SLOT_Y[h.slot];
    const hx = left ? HUB_X.left : HUB_X.right;
    const c = centerCurve(h);
    const hubDim = dim(h.nodeIds);
    links.push({ key: `c-${h.id}`, ...c, color: h.color, on: hubActive(h) || selection.kind === "center", dim: hubDim });

    cards.push(
      <button
        key={`hub-${h.id}`}
        className={`cg-card cg-hub ${isSel({ kind: "hub", id: h.id }) ? "sel" : ""} ${hubDim ? "dim" : ""}`}
        style={{ left: hx - HUB.w / 2, top: hy - HUB.h / 2, width: HUB.w, height: HUB.h, ["--c" as string]: h.color }}
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
      const ly = leafY(i, n, h.slot);
      const lx = left ? LEAF_X.left : LEAF_X.right;
      const from: Pt = [left ? hx - HUB.w / 2 : hx + HUB.w / 2, hy];
      const to: Pt = [left ? lx + LEAF.w / 2 : lx - LEAF.w / 2, ly];
      const sel: Selection = l.more ? { kind: "hub", id: h.id } : { kind: "leaf", hub: h.id, leaf: l.id };
      const leafDim = dim(l.nodeIds);
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
            className={`cg-card cg-center ${selection.kind === "center" ? "sel" : ""}`}
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
