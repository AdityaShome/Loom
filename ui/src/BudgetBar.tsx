import type { NodeRow } from "./api";
import { fmtTokens, KINDS } from "./format";

export default function BudgetBar({ nodes, budget, isDefault }: { nodes: NodeRow[]; budget: number; isDefault: boolean }) {
  const byKind = new Map<string, number>();
  for (const n of nodes) byKind.set(n.kind, (byKind.get(n.kind) ?? 0) + n.tokens);
  const total = [...byKind.values()].reduce((a, b) => a + b, 0);
  const pct = Math.min(100, (total / budget) * 100);
  const over = total > budget;
  const present = KINDS.filter((k) => byKind.has(k));

  return (
    <section className="budget" aria-label="Token budget">
      <div className="budget-line">
        <strong>{fmtTokens(total)}</strong> / {fmtTokens(budget)} tokens
        {isDefault && <span className="muted"> (default budget; counts are estimates, about chars/4)</span>}
        <span className={over ? "over" : "muted"}> · {pct.toFixed(0)}%</span>
      </div>
      <div className="bar" role="img" aria-label={`${pct.toFixed(0)}% of budget used`}>
        {present.map((k) => (
          <span
            key={k}
            className={`seg k-${k}`}
            style={{ width: `${((byKind.get(k) ?? 0) / Math.max(budget, total)) * 100}%` }}
            title={`${k}: ${fmtTokens(byKind.get(k) ?? 0)} tokens`}
          />
        ))}
      </div>
      <div className="legend">
        {present.map((k) => (
          <span key={k}>
            <i className={`dot k-${k}`} /> {k} {fmtTokens(byKind.get(k) ?? 0)}
          </span>
        ))}
      </div>
    </section>
  );
}
