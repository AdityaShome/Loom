import { useEffect, useMemo, useState } from "react";
import { api, type Action, type NodeRow, type PlanResponse, type Weights } from "./api";
import { fmtTokens } from "./format";

const WEIGHT_FIELDS: [keyof Weights, string, string][] = [
  ["w_imp", "Importance", "weight of the importance slider"],
  ["w_rec", "Recency", "weight of exp(-age/tau)"],
  ["w_ref", "References", "weight of how often later nodes refer to it"],
  ["w_task", "Relevance", "weight of keyword overlap with the latest user message"],
  ["w_sup", "Superseded", "penalty when a newer version exists"],
  ["tau", "Tau (turns)", "recency decay constant"],
  ["summarize_threshold", "Summarize above", "minimum score worth summarizing instead of dropping"],
  ["protect_tail", "Protect last N", "newest nodes that are always kept"],
];

export default function PlanView({
  sessionId,
  resp,
  nodes,
  reload,
}: {
  sessionId: string;
  resp: PlanResponse;
  nodes: NodeRow[];
  reload: () => void;
}) {
  const { plan } = resp;
  const [filter, setFilter] = useState<"" | Action>("");
  const [budget, setBudget] = useState(String(resp.budget_is_default ? "" : resp.budget_tokens));
  const [weights, setWeights] = useState(resp.weights);
  useEffect(() => setBudget(resp.budget_is_default ? "" : String(resp.budget_tokens)), [resp.budget_is_default, resp.budget_tokens]);
  useEffect(() => setWeights(resp.weights), [resp.weights]);

  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes]);
  const counts = { Keep: 0, Summarize: 0, Drop: 0 };
  plan.items.forEach((i) => counts[i.decision]++);
  const saved = plan.original_tokens - plan.planned_tokens;
  const rows = plan.items.filter((i) => !filter || i.decision === filter);

  const run = (p: Promise<void>) => p.then(reload, (e) => alert(String(e.message ?? e)));
  const weightsChanged = JSON.stringify(weights) !== JSON.stringify(resp.weights);

  return (
    <>
      {resp.pending_changes && (
        <p className="banner">Pending changes: your edits are in this plan and apply on the next model call.</p>
      )}
      {plan.over_budget && (
        <p className="banner bad">
          Required items alone ({fmtTokens(plan.must_tokens)}) exceed the {fmtTokens(plan.budget)} budget. Everything else
          is dropped; unpin something or raise the budget.
        </p>
      )}

      <div className="settings">
        <label>
          Mode{" "}
          <select value={resp.mode} onChange={(e) => run(api.updateSession(sessionId, { mode: e.target.value }))}>
            <option value="Observe">Observe</option>
            <option value="Suggest">Suggest</option>
            <option value="Enforce" disabled>
              Enforce (not available yet)
            </option>
          </select>
        </label>
        <label>
          Budget{" "}
          <input
            type="number"
            min={0}
            value={budget}
            placeholder={`${resp.budget_tokens} (model default)`}
            onChange={(e) => setBudget(e.target.value)}
          />
        </label>
        <button onClick={() => run(api.updateSession(sessionId, { budget_tokens: Number(budget) || 0 }))}>
          {budget ? "Set budget" : "Use model default"}
        </button>
        <span className="muted">
          {resp.mode === "Suggest"
            ? "Suggest: this plan is shown here; the original request is still sent."
            : "Observe: recording only. The plan below is a preview."}
        </span>
      </div>

      <div className="cards">
        <div>
          <span className="muted">Now</span>
          <strong>{fmtTokens(plan.original_tokens)}</strong>
        </div>
        <div>
          <span className="muted">Planned</span>
          <strong>{fmtTokens(plan.planned_tokens)}</strong>
        </div>
        <div>
          <span className="muted">Saved</span>
          <strong>
            {fmtTokens(saved)} ({plan.original_tokens ? Math.round((saved / plan.original_tokens) * 100) : 0}%)
          </strong>
        </div>
        <div>
          <span className="muted">Required floor</span>
          <strong>{fmtTokens(plan.must_tokens)}</strong>
        </div>
        <div>
          <span className="muted">Keep / Summarize / Drop</span>
          <strong>
            {counts.Keep} / {counts.Summarize} / {counts.Drop}
          </strong>
        </div>
      </div>
      <p className="muted small">
        Summary sizes are estimates until the summarizer exists. Prompt-cache prefix: first {plan.cache_prefix_len} nodes are
        unchanged since the last call;{" "}
        {plan.prefix_frozen ? "they are held fixed to protect caching." : "they are not held fixed (nothing to protect, or the budget forces edits)."}
      </p>

      <details className="weights">
        <summary>Scoring weights</summary>
        <div className="wgrid">
          {WEIGHT_FIELDS.map(([k, label, help]) => (
            <label key={k} title={help}>
              {label}
              <input
                type="number"
                step="any"
                value={weights[k]}
                onChange={(e) => setWeights({ ...weights, [k]: Number(e.target.value) })}
              />
            </label>
          ))}
        </div>
        <button disabled={!weightsChanged} onClick={() => run(api.updateSession(sessionId, { weights }))}>
          Save weights
        </button>{" "}
        <button disabled={!weightsChanged} onClick={() => setWeights(resp.weights)}>
          Reset
        </button>
        <p className="muted small">score = importance + recency + references + relevance − superseded (each times its weight)</p>
      </details>

      <div className="filters">
        <select value={filter} onChange={(e) => setFilter(e.target.value as "" | Action)} aria-label="Filter by decision">
          <option value="">All decisions</option>
          <option value="Keep">Keep</option>
          <option value="Summarize">Summarize</option>
          <option value="Drop">Drop</option>
        </select>
        <span className="muted">{rows.length} shown</span>
      </div>
      <table className="grid">
        <thead>
          <tr>
            <th>#</th>
            <th>Kind</th>
            <th>Content</th>
            <th>Decision</th>
            <th>Why</th>
            <th className="num">Tokens</th>
            <th className="num">Score</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((i) => {
            const n = byId.get(i.id);
            return (
              <tr key={i.id} className={i.decision === "Keep" ? "" : "shrunk"}>
                <td>{(n?.position ?? 0) + 1}</td>
                <td>
                  <span className={`badge k-${n?.kind}`}>{n?.kind}</span>
                </td>
                <td className="content">
                  {n?.source && <code className="src">{n.source}</code>}
                  <span className="preview">{n?.preview}</span>
                </td>
                <td>
                  <span className={`dec d-${i.decision}`}>{i.decision}</span>
                </td>
                <td className="why">{i.reason}</td>
                <td className="num">
                  {i.decision === "Keep" ? fmtTokens(i.tokens) : `${fmtTokens(i.tokens)} → ${fmtTokens(i.planned_tokens)}`}
                </td>
                <td className="num">{i.score.toFixed(2)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </>
  );
}
