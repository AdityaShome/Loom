import { lazy, Suspense, useMemo, useState } from "react";
import { api } from "./api";
import { useLive, useLoad } from "./hooks";
import { fmtTokens } from "./format";
import BudgetBar from "./BudgetBar";
import ContextGraph from "./ContextGraph";
import ContextList from "./ContextList";
import NodeEditor from "./NodeEditor";
import PlanView from "./PlanView";
import RulesView from "./RulesView";
import TurnDiff from "./TurnDiff";

// The knowledge graph uses Cytoscape, which is large: load it only when the tab opens.
const KnowledgeView = lazy(() => import("./KnowledgeView"));

type Tab = "context" | "plan" | "graph" | "knowledge" | "rules" | "diff";
const TABS: [Tab, string][] = [
  ["context", "Context"],
  ["plan", "Plan"],
  ["graph", "Graph"],
  ["knowledge", "Knowledge"],
  ["rules", "Rules"],
  ["diff", "Turn diff"],
];

export default function SessionView({ id }: { id: string }) {
  const [tab, setTab] = useState<Tab>(() => {
    const t = new URLSearchParams(location.hash.split("?")[1]).get("tab");
    return TABS.some(([k]) => k === t) ? (t as Tab) : "context";
  });
  const [tick, setTick] = useState(0);
  const [editNode, setEditNode] = useState<string | null>(null);
  const sessions = useLoad(api.sessions, [id]);
  const nodes = useLoad(() => api.nodes(id), [id]);
  const turns = useLoad(() => api.turns(id), [id]);
  const plan = useLoad(() => api.plan(id), [id]);

  const reloadAll = () => {
    sessions.reload();
    nodes.reload();
    turns.reload();
    plan.reload();
    setTick((t) => t + 1);
  };
  // turn_completed and plan_updated both refresh everything.
  const connected = useLive(id, reloadAll);

  const planById = useMemo(() => new Map((plan.data?.plan.items ?? []).map((i) => [i.id, i])), [plan.data]);

  const session = sessions.data?.find((s) => s.id === id);
  const err = nodes.error ?? turns.error;
  if (err) return <p className="error">{err}</p>;
  if (!nodes.data || !turns.data) return <p className="muted">Loading…</p>;

  const latest = turns.data.length ? turns.data[turns.data.length - 1].idx : -1;
  const inContext = nodes.data.filter((n) => n.last_seen_turn === latest && n.status !== "DroppedByAgent");
  const pinnedTokens = inContext.filter((n) => n.pinned).reduce((s, n) => s + n.tokens, 0);

  return (
    <>
      <div className="head">
        <a href="#/">← Sessions</a>
        <h2>{session?.name ?? id}</h2>
        <span className="muted">
          {session?.model ?? "unknown model"} · turn {latest + 1} · mode {plan.data?.mode ?? session?.mode ?? "Observe"}
          {pinnedTokens > 0 && ` · ${fmtTokens(pinnedTokens)} tokens pinned`}
        </span>
        {plan.data?.pending_changes && <span className="pending">pending changes</span>}
        <span className={connected ? "live on" : "live"}>{connected ? "● live" : "○ reconnecting"}</span>
      </div>

      <BudgetBar
        nodes={inContext}
        budget={plan.data?.budget_tokens ?? 200_000}
        isDefault={plan.data?.budget_is_default ?? true}
      />

      <nav className="tabs">
        {TABS.map(([k, label]) => (
          <button key={k} className={tab === k ? "active" : ""} onClick={() => setTab(k)}>
            {label}
            {k === "context" && <span className="count"> {inContext.length}</span>}
          </button>
        ))}
      </nav>

      {tab === "context" && <ContextList nodes={nodes.data} latest={latest} plan={planById} onEdit={reloadAll} onEditText={setEditNode} />}
      {tab === "plan" &&
        (plan.data ? (
          <PlanView sessionId={id} resp={plan.data} nodes={nodes.data} reload={reloadAll} />
        ) : (
          <p className={plan.error ? "error" : "muted"}>{plan.error ?? "Loading…"}</p>
        ))}
      {tab === "graph" && (
        <ContextGraph
          sessionId={id}
          session={session}
          plan={plan.data}
          turns={turns.data}
          latest={latest}
          tick={tick}
          connected={connected}
        />
      )}
      {tab === "knowledge" && (
        <Suspense fallback={<p className="muted">Loading…</p>}>
          <KnowledgeView sessionId={id} mode={plan.data?.mode ?? session?.mode ?? "Observe"} tick={tick} onEditText={setEditNode} />
        </Suspense>
      )}
      {tab === "rules" && <RulesView sessionId={id} tick={tick} />}
      {tab === "diff" && <TurnDiff sessionId={id} turns={turns.data} nodes={nodes.data} />}
      {editNode && (
        <NodeEditor
          key={editNode}
          nodeId={editNode}
          mode={plan.data?.mode ?? session?.mode ?? "Observe"}
          onClose={() => setEditNode(null)}
          onChanged={reloadAll}
        />
      )}
    </>
  );
}
