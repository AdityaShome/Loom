import { useEffect, useRef, useState } from "react";
import { api, type Action, type NodeRow, type PlanItem } from "./api";
import { fmtTokens } from "./format";

/** Per-node user controls: pin (with its token price), importance, forced action, restore. */
export default function Controls({ n, item, onEdit }: { n: NodeRow; item?: PlanItem; onEdit: () => void }) {
  const [imp, setImp] = useState(n.importance);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => setImp(n.importance), [n.importance]);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const run = (p: Promise<void>) => p.then(onEdit, (e) => alert(String(e.message ?? e)));
  const slide = (v: number) => {
    setImp(v);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => run(api.patchNode(n.id, { importance: v })), 250);
  };
  const shrunk = item && item.decision !== "Keep" && n.user_action !== "Keep";

  return (
    <div className="controls">
      <button
        className={n.pinned ? "pin on" : "pin"}
        title={`Pinned nodes are always sent. Cost: ${n.tokens} tokens on every call.`}
        aria-pressed={n.pinned}
        onClick={() => run(api.patchNode(n.id, { pinned: !n.pinned }))}
      >
        {n.pinned ? "Pinned" : "Pin"} · {fmtTokens(n.tokens)}
      </button>
      <label className="imp" title="Importance (0–1) feeds the score">
        <input
          type="range"
          min={0}
          max={1}
          step={0.05}
          value={imp}
          onChange={(e) => slide(Number(e.target.value))}
          aria-label="Importance"
        />
        <span>{imp.toFixed(2)}</span>
      </label>
      <select
        value={n.user_action ?? ""}
        aria-label="Force action"
        onChange={(e) => run(api.patchNode(n.id, { user_action: (e.target.value || null) as Action | null }))}
      >
        <option value="">Auto</option>
        <option value="Keep">Keep</option>
        <option value="Summarize">Summarize</option>
        <option value="Drop">Drop</option>
      </select>
      {shrunk && <button onClick={() => run(api.restore(n.id))}>Restore</button>}
    </div>
  );
}
