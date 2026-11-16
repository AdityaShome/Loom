import { useEffect, useMemo, useState } from "react";
import { api, type Diff, type NodeRow, type Turn } from "./api";
import { fmtTokens } from "./format";

export default function TurnDiff({ sessionId, turns, nodes }: { sessionId: string; turns: Turn[]; nodes: NodeRow[] }) {
  const last = turns.length ? turns[turns.length - 1].idx : 0;
  const [a, setA] = useState(Math.max(0, last - 1));
  const [b, setB] = useState(last);
  const [diff, setDiff] = useState<Diff | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Follow the live edge until the user picks turns explicitly.
  const [picked, setPicked] = useState(false);

  useEffect(() => {
    if (!picked) {
      setA(Math.max(0, last - 1));
      setB(last);
    }
  }, [last, picked]);

  useEffect(() => {
    if (!turns.length) return;
    api.diff(sessionId, a, b).then(
      (d) => {
        setDiff(d);
        setError(null);
      },
      (e) => setError(String(e.message ?? e)),
    );
  }, [sessionId, a, b, turns.length]);

  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes]);
  if (!turns.length) return <p className="muted">No turns yet.</p>;

  const tok = (ids: string[]) => ids.reduce((s, i) => s + (byId.get(i)?.tokens ?? 0), 0);
  const ta = turns.find((t) => t.idx === a);
  const tb = turns.find((t) => t.idx === b);

  const pick = (set: (n: number) => void) => (e: React.ChangeEvent<HTMLSelectElement>) => {
    setPicked(true);
    set(Number(e.target.value));
  };
  const opts = turns.map((t) => (
    <option key={t.idx} value={t.idx}>
      Turn {t.idx + 1} · {fmtTokens(t.request_tokens)} tokens
    </option>
  ));

  const list = (ids: string[], cls: string, sign: string) =>
    ids.length === 0 ? (
      <p className="muted">None</p>
    ) : (
      <ul className="difflist">
        {ids.map((id) => {
          const n = byId.get(id);
          return (
            <li key={id} className={cls}>
              <span className="sign">{sign}</span>
              <span className={`badge k-${n?.kind}`}>{n?.kind ?? "?"}</span>
              <span className="preview">{n?.preview || n?.source || id}</span>
              <span className="num muted">{fmtTokens(n?.tokens ?? 0)}</span>
            </li>
          );
        })}
      </ul>
    );

  const delta = ta && tb ? tb.request_tokens - ta.request_tokens : 0;

  return (
    <>
      <div className="filters">
        <label>
          From{" "}
          <select value={a} onChange={pick(setA)}>
            {opts}
          </select>
        </label>
        <label>
          To{" "}
          <select value={b} onChange={pick(setB)}>
            {opts}
          </select>
        </label>
        {picked && <button onClick={() => setPicked(false)}>follow latest</button>}
        {ta && tb && (
          <span className="muted">
            {fmtTokens(ta.request_tokens)} → {fmtTokens(tb.request_tokens)} tokens ({delta >= 0 ? "+" : "−"}
            {fmtTokens(Math.abs(delta))})
          </span>
        )}
      </div>
      {error && <p className="error">{error}</p>}
      {diff && (
        <div className="diff">
          <div>
            <h3>
              Added <span className="count">{diff.added.length}</span>{" "}
              <span className="muted">{fmtTokens(tok(diff.added))} tokens</span>
            </h3>
            {list(diff.added, "add", "+")}
          </div>
          <div>
            <h3>
              Removed <span className="count">{diff.removed.length}</span>{" "}
              <span className="muted">{fmtTokens(tok(diff.removed))} tokens</span>
            </h3>
            {list(diff.removed, "del", "−")}
          </div>
        </div>
      )}
    </>
  );
}
