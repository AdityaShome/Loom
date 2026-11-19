# Loom

A control plane for agent context. Loom sits between an agent and its model API, shows what the agent's context holds right now, and lets you pin, weight, summarize or drop items before each call.

It works with any agent framework without code changes: point the agent's base URL at Loom.

> **Status: early.** Loom records and visualizes context, builds a relation graph, and computes a plan for what it *would* keep, summarize or drop. It does **not** yet rewrite requests: the model always receives the original request. Enforce mode (rewriting) is not implemented.

## What works today

- **Transparent proxy** for Anthropic (`/v1/messages`) and OpenAI-style (`/v1/chat/completions`, `/v1/responses`, ...) APIs. Streaming responses are forwarded byte for byte. API keys pass straight through and are never stored.
- **Context tracking.** Every request is split into nodes (system, user, assistant, tool call, tool result, file, memory), deduplicated across turns, and stored in SQLite with token estimates.
- **Relation graph.** Tool call/result pairs, turn order, newer reads superseding older ones, and mentions of earlier files.
- **Policy engine (preview).** Scores every node and plans keep / summarize / drop within a token budget, with a reason per node. Hard constraints always hold: system prompt, pinned nodes, latest user message, recent context, and tool pairs stay together.
- **Web UI** served by the same binary: sessions, context list with per-node controls (pin, importance, forced action, restore), plan preview, graph, rules editor, and a turn-to-turn diff. Updates arrive live over a WebSocket.

Token counts are estimates (about 1 token per 4 characters). Summary sizes in plans are estimates too, since there is no summarizer yet.

## Quick start

Requires a recent Rust toolchain and Node.js.

```sh
# 1. Build the web UI (it is embedded into the binary)
cd ui
npm install
npm run build
cd ..

# 2. Run Loom
cargo run --release
```

Open <http://localhost:7070> for the UI, then point your agent at Loom.

**Anthropic**

```sh
export ANTHROPIC_BASE_URL=http://localhost:7070
```

**OpenAI-compatible**

```sh
export OPENAI_BASE_URL=http://localhost:7070/v1
```

Requests appear in the UI as sessions. To name a session explicitly, send an `x-loom-session` header; otherwise Loom derives one from the system prompt and first user message. The header is removed before the request is forwarded.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `LOOM_LISTEN` | `127.0.0.1:7070` | Address to listen on |
| `LOOM_ANTHROPIC_UPSTREAM` | `https://api.anthropic.com` | Upstream for Anthropic-style paths (`LOOM_UPSTREAM` also works) |
| `LOOM_OPENAI_UPSTREAM` | `https://api.openai.com` | Upstream for OpenAI-style paths |
| `LOOM_DB` | `loom.db` | SQLite file for recorded sessions |
| `RUST_LOG` | `loom_server=info` | Log filter |

Point the OpenAI upstream at a local server (Ollama, vLLM, llama.cpp) to use Loom with local models, for example `LOOM_OPENAI_UPSTREAM=http://localhost:11434`.

If the database cannot be opened, Loom logs an error and keeps proxying without recording. Internal errors in recording never affect the agent's request.

## Control API

| Endpoint | Purpose |
|---|---|
| `GET /api/sessions` | List sessions |
| `GET /api/sessions/{id}/nodes` | Nodes with status, score, tokens, controls |
| `GET /api/sessions/{id}/graph` | Nodes and edges |
| `GET /api/sessions/{id}/turns` | Request history |
| `GET /api/sessions/{id}/diff/{a}/{b}` | Nodes added and removed between two turns |
| `GET /api/sessions/{id}/plan` | Plan for the next request |
| `PUT /api/sessions/{id}` | Set budget, mode (`Observe` or `Suggest`), scoring weights |
| `PATCH /api/nodes/{id}` | Set `importance`, `pinned`, `user_action` |
| `POST /api/nodes/{id}/restore` | Force a node to be kept |
| `GET/POST /api/rules`, `PUT/DELETE /api/rules/{id}` | Policy rules |
| `POST /api/sessions/{id}/match` | Live match count for a rule matcher |
| `WS /ws/sessions/{id}` | Live events: `turn_completed`, `plan_updated` |

## Repository layout

```
crates/
  loom-core/     data model, normalization, graph, policy (no IO)
  loom-store/    SQLite persistence and migrations
  loom-server/   gateway, control API, embedded UI, main binary
ui/              React + Vite web UI
```

`loom-core` does no IO, so the policy logic is unit- and property-tested in isolation.

## Development

```sh
cargo test                 # Rust unit, property and integration tests
cd ui && npm run dev       # UI dev server on :5173, proxies /api and /ws to :7070
cd ui && npm run typecheck
```

Run `loom` on `:7070` first when using the UI dev server. In debug builds the binary serves `ui/dist` from disk, so rebuild the UI with `npm run build` to see changes there. Release builds embed `ui/dist`, so build the UI before `cargo build --release`.

## Privacy and security

- The store holds **full prompts and tool outputs** in plain text in `loom.db`. Treat it as sensitive.
- Loom binds to `127.0.0.1` by default. The control API has **no authentication**, so do not expose it to a network you do not trust.
- API keys are passed through to the upstream and never written to disk.

## Limitations

- No request rewriting yet (no Enforce mode, no summarizer).
- Token counts are rough estimates.
- Relevance scoring is keyword overlap with the latest user message.
- Relation detection links file paths and tool pairs only; URLs and error codes are not matched yet.

## License

Apache-2.0
