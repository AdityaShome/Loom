export type Session = {
  id: string;
  name: string | null;
  provider: string | null;
  model: string | null;
  mode: string;
  budget_tokens: number;
  turns: number;
  nodes: number;
  saved_tokens: number;
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
  /** You replaced the text the model sees (applied in Enforce). */
  edited: boolean;
  /** A block type that cannot be rewritten (thinking, images). */
  locked: boolean;
  status: string;
  score: number;
  preview: string;
};

export type Turn = {
  idx: number;
  /** Tokens as the agent sent them. */
  request_tokens: number;
  /** Tokens the model actually received. */
  sent_tokens: number;
  rewritten: boolean;
  created_at: number;
  nodes: number;
};
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
  /** Tokens of your notes, already taken out of the budget. */
  notes_tokens: number;
  summarizer: { model: string; provider: string } | null;
};
export type Matcher = {
  kind?: string | null;
  source_glob?: string | null;
  min_tokens?: number | null;
  older_than_turns?: number | null;
};
export type Rule = { id: string; session_id: string | null; matcher: Matcher; action: Action; priority: number };
export type EntityKind = "File" | "Symbol" | "Command" | "Error" | "Goal" | "Concept" | "Note";
export type Source = "auto" | "model" | "user";
export type Entity = { id: string; session_id: string; kind: EntityKind; name: string; note: string | null; source: Source };
export type Fact = {
  id: string;
  session_id: string;
  subject_id: string;
  predicate: string;
  object_id: string | null;
  text: string;
  source: Source;
  node_id: string | null;
  /** Sent to the model in Enforce mode. */
  inject: boolean;
};
export type Knowledge = {
  entities: Entity[];
  facts: Fact[];
  /** Exactly what would be appended to the model's request. */
  memory: { text: string; tokens: number; facts: number } | null;
  edited_nodes: string[];
};
export type NodeDetail = NodeRow & { content: string; override: string | null };
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
  knowledge: (id: string) => call<Knowledge>("GET", `/api/sessions/${enc(id)}/knowledge`),
  createEntity: (session: string, e: { kind: EntityKind; name: string; note?: string }) =>
    call<Entity>("POST", `/api/sessions/${enc(session)}/knowledge/entities`, e),
  updateEntity: (id: string, p: { name?: string; kind?: EntityKind; note?: string | null }) =>
    call<Entity>("PATCH", `/api/knowledge/entities/${enc(id)}`, p),
  deleteEntity: (id: string) => call<void>("DELETE", `/api/knowledge/entities/${enc(id)}`),
  createFact: (session: string, f: { subject_id: string; predicate: string; object_id?: string | null; text?: string; inject?: boolean }) =>
    call<Fact>("POST", `/api/sessions/${enc(session)}/knowledge/facts`, f),
  updateFact: (id: string, p: { subject_id?: string; predicate?: string; object_id?: string | null; text?: string; inject?: boolean }) =>
    call<Fact>("PATCH", `/api/knowledge/facts/${enc(id)}`, p),
  deleteFact: (id: string) => call<void>("DELETE", `/api/knowledge/facts/${enc(id)}`),
  nodeDetail: (id: string) => call<NodeDetail>("GET", `/api/nodes/${enc(id)}`),
  setNodeText: (id: string, text: string) => call<void>("PUT", `/api/nodes/${enc(id)}/content`, { text }),
  clearNodeText: (id: string) => call<void>("DELETE", `/api/nodes/${enc(id)}/content`),
  match: (session: string, m: Matcher) => call<MatchResult>("POST", `/api/sessions/${enc(session)}/match`, m),
};
