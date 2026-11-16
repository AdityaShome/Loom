import { lazy, Suspense, useState } from "react";
import { api } from "./api";
import { useLive, useLoad } from "./hooks";
import BudgetBar from "./BudgetBar";
import ContextList from "./ContextList";
import TurnDiff from "./TurnDiff";

// Cytoscape is large; load it only when the Graph tab opens.
const GraphView = lazy(() => import("./GraphView"));

const DEFAULT_BUDGET = 200_000;

export default function SessionView({ id }: { id: string }) {
  const [tab, setTab] = useState<"context" | "graph" | "diff">(() => {
    const t = new URLSearchParams(location.hash.split("?")[1]).get("tab");
    return t === "graph" || t === "diff" ? t : "context";
  });
  const [tick, setTick] = useState(0);
  const sessions = useLoad(api.sessions, [id]);
  const nodes = useLoad(() => api.nodes(id), [id]);
  const turns = useLoad(() => api.turns(id), [id]);

  const connected = useLive(id, () => {
    sessions.reload();
    nodes.reload();
    turns.reload();
    setTick((t) => t + 1);
  });

  const session = sessions.data?.find((s) => s.id === id);
  const err = nodes.error ?? turns.error;
  if (err) return <p className="error">{err}</p>;
  if (!nodes.data || !turns.data) return <p className="muted">Loading…</p>;

  const latest = turns.data.length ? turns.data[turns.data.length - 1].idx : -1;
  const inContext = nodes.data.filter((n) => n.last_seen_turn === latest && n.status !== "DroppedByAgent");
  const budget = session?.budget_tokens || DEFAULT_BUDGET;

  return (
    <>
      <div className="head">
        <a href="#/">← Sessions</a>
        <h2>{session?.name ?? id}</h2>
        <span className="muted">
          {session?.model ?? "unknown model"} · turn {latest + 1} · mode {session?.mode ?? "Observe"}
        </span>
        <span className={connected ? "live on" : "live"}>{connected ? "● live" : "○ reconnecting"}</span>
      </div>

      <BudgetBar nodes={inContext} budget={budget} isDefault={!session?.budget_tokens} />

      <nav className="tabs">
        <button className={tab === "context" ? "active" : ""} onClick={() => setTab("context")}>
          Context <span className="count">{inContext.length}</span>
        </button>
        <button className={tab === "graph" ? "active" : ""} onClick={() => setTab("graph")}>
          Graph
        </button>
        <button className={tab === "diff" ? "active" : ""} onClick={() => setTab("diff")}>
          Turn diff
        </button>
      </nav>

      {tab === "context" ? (
        <ContextList nodes={nodes.data} latest={latest} />
      ) : tab === "graph" ? (
        <Suspense fallback={<p className="muted">Loading graph…</p>}>
          <GraphView sessionId={id} tick={tick} latest={latest} />
        </Suspense>
      ) : (
        <TurnDiff sessionId={id} turns={turns.data} nodes={nodes.data} />
      )}
    </>
  );
}
