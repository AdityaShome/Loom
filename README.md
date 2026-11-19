# Loom

A control plane for agent context. Loom sits between an agent and its model API, shows what the agent's context holds right now, and lets you pin, weight, summarize or drop items before each call.

It works with any agent framework without code changes: point the agent's base URL at Loom.

> **Status: early.** Loom records and visualizes context, plans what to keep, summarize or drop, and (in **Enforce** mode) rewrites requests to match the plan before they are sent. Every session starts in **Observe** mode, where the model always receives the original request. Enforce has only been tried against small local models so far.

## Functions

- **Transparent proxy** for Anthropic (`/v1/messages`) and OpenAI-style (`/v1/chat/completions`, `/v1/responses`, ...) APIs. Streaming responses are forwarded byte for byte. API keys pass straight through and are never stored.
- **Context tracking.** Every request is split into nodes (system, user, assistant, tool call, tool result, file, memory), deduplicated across turns, and stored in SQLite with token estimates.
- **Relation graph.** Tool call/result pairs, turn order, newer reads superseding older ones, and mentions of earlier files.
- **Policy engine.** Scores every node and plans keep / summarize / drop within a token budget, with a reason per node. Hard constraints always hold: system prompt, pinned nodes, latest user message, recent context, tool pairs, and blocks that cannot be edited safely (thinking, images).
- **Modes.** `Observe` records only. `Suggest` also shows the plan and prepares summaries, but still sends the original request. `Enforce` rewrites the request: dropped nodes are removed, summarized tool results and messages are replaced by short summaries, and the result is validated (role order, tool call/result pairing) before sending. If anything fails, or the plan changes nothing, the original request is sent unchanged.
- **Summaries.** Produced by a model you configure, cached by content hash, and generated in the background: a request never waits longer than `LOOM_SUMMARIZER_TIMEOUT_MS` (default 300 ms). A summary that is not ready is skipped for that call (the node is kept if it fits, else dropped) and used on the next one.
- **Prompt-cache friendly.** Decisions for the part of the request that was already sent are repeated, so the provider's prompt cache stays valid, unless the budget forces changes.
- **Web UI** served by the same binary: sessions, context list with per-node controls (pin, importance, forced action, restore), plan preview, graph, rules editor, and a turn-to-turn diff. Updates arrive live over a WebSocket.

Token counts are estimates (about 1 token per 4 characters). Plans use real summary sizes once a summary exists and estimates before that.

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
| `LOOM_SUMMARIZER_MODEL` | unset | Model used for summaries. Without it, Enforce can only keep or drop |
| `LOOM_SUMMARIZER_PROVIDER` | `openai` | `openai` (also Ollama, vLLM, llama.cpp) or `anthropic` |
| `LOOM_SUMMARIZER_URL` | the provider's upstream | Base URL of the summarizer |
| `LOOM_SUMMARIZER_API_KEY` | unset | Key for the summarizer. If unset, the triggering request's own credentials are reused when the provider matches (held in memory only) |
| `LOOM_SUMMARIZER_TIMEOUT_MS` | `300` | How long a request may wait for summaries |
| `LOOM_SUMMARIZER_EXTRA` | unset | JSON merged into the summarizer request body (default for Qwen3 models: `{"reasoning_effort":"none"}`) |
| `RUST_LOG` | `loom_server=info` | Log filter |

### Trying it with a local model

Point the OpenAI upstream at a local server (Ollama, vLLM, llama.cpp), and optionally use a local model for summaries too:

```sh
ollama pull qwen3:4b
export LOOM_OPENAI_UPSTREAM=http://localhost:11434
export LOOM_SUMMARIZER_MODEL=qwen3:4b
export LOOM_SUMMARIZER_URL=http://localhost:11434
cargo run --release

# in another terminal: a small read-only tool-using agent that talks to Loom
python examples/agent.py --model qwen3:4b "Explain how session ids are resolved"
```

Then open the session in the UI, set the mode to `Enforce` and a small budget in the Plan tab, and compare what the agent sent with what the model received in the Turn diff tab. A local summarizer can be slow (tens of seconds on a small GPU), so the first call after a change falls back and later calls use the summaries. Set `OLLAMA_CONTEXT_LENGTH` high enough for your prompts, or Ollama silently truncates them.

If the database cannot be opened, Loom logs an error and keeps proxying without recording. Internal errors in recording never affect the agent's request.

## Control API

| Endpoint | Purpose |
|---|---|
| `GET /api/sessions` | List sessions |
| `GET /api/sessions/{id}/nodes` | Nodes with status, score, tokens, controls |
| `GET /api/sessions/{id}/graph` | Nodes and edges |
| `GET /api/sessions/{id}/turns` | Request history: tokens the agent sent vs what the model received |
| `GET /api/sessions/{id}/diff/{a}/{b}` | Nodes added and removed between two turns |
| `GET /api/sessions/{id}/plan` | Plan for the next request |
| `PUT /api/sessions/{id}` | Set budget, mode (`Observe`, `Suggest` or `Enforce`), scoring weights |
| `PATCH /api/nodes/{id}` | Set `importance`, `pinned`, `user_action` |
| `POST /api/nodes/{id}/restore` | Force a node to be kept |
| `GET/POST /api/rules`, `PUT/DELETE /api/rules/{id}` | Policy rules |
| `POST /api/sessions/{id}/match` | Live match count for a rule matcher |
| `WS /ws/sessions/{id}` | Live events: `turn_completed`, `plan_updated` |

## Repository layout

```
crates/
  loom-core/     data model, normalization, graph, policy, rewriter (no IO)
  loom-store/    SQLite persistence and migrations
  loom-server/   gateway, enforce pipeline, summarizer, control API, embedded UI, main binary
ui/              React + Vite web UI
examples/        agent.py: a tiny tool-using agent for trying Loom with any OpenAI-compatible model
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
- In Enforce mode Loom changes what the model sees. Dropped or summarized context can make an agent lose details; originals are never deleted from the store, and `Restore` or a pin brings a node back on the next call.
- Summaries are sent to the summarizer model, so they leave the machine if that model is remote.
- Loom binds to `127.0.0.1` by default. The control API has **no authentication**, so do not expose it to a network you do not trust.
- API keys are passed through to the upstream and never written to disk.

## Limitations

- Enforce has only been exercised against small local models and fake upstreams, not against the Anthropic or OpenAI APIs.
- Tool-call arguments are never summarized (only results and messages are), and no marker is left where nodes were dropped.
- The per-request cost of Enforce (ingest, plan, rewrite) is not benchmarked; it measured about 20 ms in a debug build.
- Token counts are rough estimates.
- Relevance scoring is keyword overlap with the latest user message.
- Relation detection links file paths and tool pairs only; URLs and error codes are not matched yet.

## License

Apache-2.0
