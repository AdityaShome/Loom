import { api } from "./api";
import { useInterval, useLoad } from "./hooks";
import { fmtTime, fmtTokens } from "./format";

export default function Sessions() {
  const { data, error, reload } = useLoad(api.sessions, []);
  useInterval(reload, 3000);

  if (error) return <p className="error">{error}</p>;
  if (!data) return <p className="muted">Loading…</p>;
  if (data.length === 0)
    return (
      <div className="empty">
        <h2>No sessions yet</h2>
        <p>
          Point an agent at <code>http://localhost:7070</code> (set <code>ANTHROPIC_BASE_URL</code> or the OpenAI
          base URL). Sessions appear here as requests arrive.
        </p>
      </div>
    );
  return (
    <table className="grid">
      <thead>
        <tr>
          <th>Session</th>
          <th>Model</th>
          <th>Provider</th>
          <th className="num">Turns</th>
          <th className="num">Nodes</th>
          <th className="num">Saved</th>
          <th>Mode</th>
          <th>Started</th>
        </tr>
      </thead>
      <tbody>
        {data.map((s) => (
          <tr key={s.id}>
            <td>
              <a href={`#/s/${encodeURIComponent(s.id)}`}>{s.name ?? s.id}</a>
            </td>
            <td>{s.model ?? "—"}</td>
            <td>{s.provider ?? "—"}</td>
            <td className="num">{s.turns}</td>
            <td className="num">{s.nodes}</td>
            <td className="num">{s.saved_tokens > 0 ? fmtTokens(s.saved_tokens) : <span className="muted">—</span>}</td>
            <td>{s.mode}</td>
            <td className="muted">{fmtTime(s.created_at)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
