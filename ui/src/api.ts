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

export type Action = "Keep" | "Summarize" | "Drop";

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
  user_action: Action | null;
  status: string;
  score: number;
  preview: string;
};

export type Turn = { idx: number; request_tokens: number; created_at: number; nodes: number };
export type Diff = { added: string[]; removed: string[] };
export type EdgeKind = "ToolPair" | "Follows" | "RefersTo" | "Supersedes" | "SummaryOf" | "DerivedFrom";
export type Edge = { from: string; to: string; kind: EdgeKind; weight: number };
export type Graph = { nodes: NodeRow[]; edges: Edge[] };
export type LiveEvent = { type: string; turn: number; new: number; missing: number; tokens: number };

export type PlanItem = {
  id: string;
  decision: Action;
  reason: string;
  score: number;
  tokens: number;
  planned_tokens: number;
  required: boolean;
};
export type Plan = {
  budget: number;
  items: PlanItem[];
  original_tokens: number;
  planned_tokens: number;
  must_tokens: number;
  over_budget: boolean;
  cache_prefix_len: number;
  prefix_frozen: boolean;
};
export type Weights = {
  w_imp: number;
  w_rec: number;
  w_ref: number;
  w_task: number;
  w_sup: number;
  tau: number;
  summarize_threshold: number;
  protect_tail: number;
};
export type PlanResponse = {
  plan: Plan;
  mode: string;
  budget_tokens: number;
  budget_is_default: boolean;
  weights: Weights;
  pending_changes: boolean;
};
export type Matcher = {
  kind?: string | null;
  source_glob?: string | null;
  min_tokens?: number | null;
  older_than_turns?: number | null;
};
export type Rule = { id: string; session_id: string | null; matcher: Matcher; action: Action; priority: number };
export type MatchResult = { count: number; tokens: number; ids: string[] };

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const r = await fetch(path, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!r.ok) throw new Error((await r.text()) || `${method} ${path}: ${r.status}`);
  return r.status === 204 ? (undefined as T) : r.json();
}

const enc = encodeURIComponent;
export const api = {
  sessions: () => call<Session[]>("GET", "/api/sessions"),
  nodes: (id: string) => call<NodeRow[]>("GET", `/api/sessions/${enc(id)}/nodes`),
  graph: (id: string) => call<Graph>("GET", `/api/sessions/${enc(id)}/graph`),
  turns: (id: string) => call<Turn[]>("GET", `/api/sessions/${enc(id)}/turns`),
  diff: (id: string, a: number, b: number) => call<Diff>("GET", `/api/sessions/${enc(id)}/diff/${a}/${b}`),
  plan: (id: string) => call<PlanResponse>("GET", `/api/sessions/${enc(id)}/plan`),
  updateSession: (id: string, u: { budget_tokens?: number; mode?: string; weights?: Partial<Weights> }) =>
    call<void>("PUT", `/api/sessions/${enc(id)}`, u),
  patchNode: (id: string, p: { importance?: number; pinned?: boolean; user_action?: Action | null }) =>
    call<void>("PATCH", `/api/nodes/${enc(id)}`, p),
  restore: (id: string) => call<void>("POST", `/api/nodes/${enc(id)}/restore`),
  rules: (session: string) => call<Rule[]>("GET", `/api/rules?session=${enc(session)}`),
  createRule: (r: { session_id: string | null; matcher: Matcher; action: Action; priority: number }) =>
    call<Rule>("POST", "/api/rules", r),
  deleteRule: (id: string) => call<void>("DELETE", `/api/rules/${enc(id)}`),
  match: (session: string, m: Matcher) => call<MatchResult>("POST", `/api/sessions/${enc(session)}/match`, m),
};
