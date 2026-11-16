import { useEffect, useState } from "react";
import { api, type Action, type Matcher, type MatchResult, type Rule } from "./api";
import { useLoad } from "./hooks";
import { fmtTokens, KINDS } from "./format";

const describe = (m: Matcher) =>
  [
    m.kind && `kind is ${m.kind}`,
    m.source_glob && `source matches ${m.source_glob}`,
    m.min_tokens != null && `more than ${m.min_tokens} tokens`,
    m.older_than_turns != null && `older than ${m.older_than_turns} turns`,
  ]
    .filter(Boolean)
    .join(" and ");

export default function RulesView({ sessionId, tick }: { sessionId: string; tick: number }) {
  const rules = useLoad(() => api.rules(sessionId), [sessionId, tick]);
  const [kind, setKind] = useState("");
  const [glob, setGlob] = useState("");
  const [minTokens, setMinTokens] = useState("");
  const [older, setOlder] = useState("");
  const [action, setAction] = useState<Action>("Summarize");
  const [priority, setPriority] = useState("0");
  const [scope, setScope] = useState<"session" | "all">("session");
  const [hit, setHit] = useState<MatchResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const matcher: Matcher = {
    kind: kind || null,
    source_glob: glob.trim() || null,
    min_tokens: minTokens === "" ? null : Number(minTokens),
    older_than_turns: older === "" ? null : Number(older),
  };
  const empty = !matcher.kind && !matcher.source_glob && matcher.min_tokens == null && matcher.older_than_turns == null;

  // Live match count against the next request, debounced.
  const key = JSON.stringify(matcher);
  useEffect(() => {
    if (empty) return setHit(null);
    const t = window.setTimeout(() => api.match(sessionId, matcher).then(setHit, () => setHit(null)), 250);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, sessionId, tick]);

  const add = () =>
    api
      .createRule({ session_id: scope === "session" ? sessionId : null, matcher, action, priority: Number(priority) || 0 })
      .then(() => {
        setError(null);
        rules.reload();
      }, (e) => setError(String(e.message ?? e)));

  return (
    <>
      <p className="muted small">
        Rules force an action on matching nodes before scoring. Precedence: required nodes (system prompt, pins, latest user
        message, recent context) &gt; your per-node choice &gt; rules (highest priority) &gt; the scoring plan.
      </p>
      <form
        className="ruleform"
        onSubmit={(e) => {
          e.preventDefault();
          if (!empty) add();
        }}
      >
        <label>
          Kind
          <select value={kind} onChange={(e) => setKind(e.target.value)}>
            <option value="">any</option>
            {KINDS.map((k) => (
              <option key={k}>{k}</option>
            ))}
          </select>
        </label>
        <label>
          Source glob
          <input value={glob} onChange={(e) => setGlob(e.target.value)} placeholder="*.log" />
        </label>
        <label>
          More than (tokens)
          <input type="number" min={0} value={minTokens} onChange={(e) => setMinTokens(e.target.value)} placeholder="4000" />
        </label>
        <label>
          Older than (turns)
          <input type="number" min={0} value={older} onChange={(e) => setOlder(e.target.value)} placeholder="5" />
        </label>
        <label>
          Action
          <select value={action} onChange={(e) => setAction(e.target.value as Action)}>
            <option>Summarize</option>
            <option>Drop</option>
            <option>Keep</option>
          </select>
        </label>
        <label>
          Priority
          <input type="number" value={priority} onChange={(e) => setPriority(e.target.value)} />
        </label>
        <label>
          Applies to
          <select value={scope} onChange={(e) => setScope(e.target.value as "session" | "all")}>
            <option value="session">this session</option>
            <option value="all">all sessions</option>
          </select>
        </label>
        <button type="submit" disabled={empty}>
          Add rule
        </button>
        <span className="matchcount">
          {empty ? (
            <span className="muted">Set at least one condition.</span>
          ) : hit ? (
            <>
              Matches <strong>{hit.count}</strong> node{hit.count === 1 ? "" : "s"} ({fmtTokens(hit.tokens)} tokens) in the next request
            </>
          ) : (
            <span className="muted">…</span>
          )}
        </span>
      </form>
      {error && <p className="error">{error}</p>}

      {rules.data && rules.data.length === 0 && <p className="muted">No rules yet.</p>}
      {rules.data && rules.data.length > 0 && (
        <table className="grid">
          <thead>
            <tr>
              <th>When</th>
              <th>Do</th>
              <th className="num">Priority</th>
              <th>Scope</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {rules.data.map((r: Rule) => (
              <tr key={r.id}>
                <td>{describe(r.matcher)}</td>
                <td>
                  <span className={`dec d-${r.action}`}>{r.action}</span>
                </td>
                <td className="num">{r.priority}</td>
                <td className="muted">{r.session_id ? "this session" : "all sessions"}</td>
                <td>
                  <button onClick={() => api.deleteRule(r.id).then(rules.reload, (e) => alert(String(e.message ?? e)))}>
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}
