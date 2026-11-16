export type Session = {
  id: string;
  name: string | null;
  provider: string | null;
  model: string | null;
  mode: string;
  budget_tokens: number;
  turns: number;
  nodes: number;
  created_at: number;
};

export type NodeRow = {
  id: string;
  kind: string;
  role: string;
  tokens: number;
  first_seen_turn: number;
  last_seen_turn: number;
  position: number;
  source: string | null;
  importance: number;
  pinned: boolean;
  status: string;
  score: number;
  preview: string;
};

export type Turn = { idx: number; request_tokens: number; created_at: number; nodes: number };
export type Diff = { added: string[]; removed: string[] };
export type LiveEvent = { type: string; turn: number; new: number; missing: number; tokens: number };

async function get<T>(path: string): Promise<T> {
  const r = await fetch(path);
  if (!r.ok) throw new Error(`${path}: ${r.status} ${await r.text()}`);
  return r.json();
}

const enc = encodeURIComponent;
export const api = {
  sessions: () => get<Session[]>("/api/sessions"),
  nodes: (id: string) => get<NodeRow[]>(`/api/sessions/${enc(id)}/nodes`),
  turns: (id: string) => get<Turn[]>(`/api/sessions/${enc(id)}/turns`),
  diff: (id: string, a: number, b: number) => get<Diff>(`/api/sessions/${enc(id)}/diff/${a}/${b}`),
};
