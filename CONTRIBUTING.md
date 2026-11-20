# Contributing to Loom

Thanks for helping out. Loom is early, so small, focused changes with tests are the easiest to review.

## Setup

You need a recent stable Rust toolchain and Node.js 20 or newer.

```sh
git clone https://github.com/AdityaShome/Loom
cd Loom
(cd ui && npm install && npm run build)   # the UI is embedded into the binary
cargo run --release                        # http://localhost:7070
```

## Before you open a pull request

CI runs the same checks:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
(cd ui && npm run typecheck && npm test && npm run build)
```

`cargo fmt` fixes formatting (settings are in `rustfmt.toml`).

## Where things live

- `crates/loom-core`: data model, normalization, graph, policy and rewriter. **No IO**, so logic here must stay pure and unit- or property-testable.
- `crates/loom-store`: SQLite persistence. Schema changes are new files in `migrations/`; never edit a migration that has shipped.
- `crates/loom-server`: gateway, enforce pipeline, summarizer, control API and the embedded UI.
- `ui`: React + Vite. `npm run dev` serves on :5173 and proxies `/api` and `/ws` to a running Loom on :7070.

## Guidelines

- **Fail open.** If Loom cannot do its job (store error, bad plan, invalid rewrite, slow summarizer), the agent's original request must still go through. Add a test when you touch these paths.
- **Never store API keys.** Credentials pass through to the upstream and may be held in memory only.
- **Test behavior, not just code paths.** The rewriter and policy have property tests: if you change them, run `cargo test` several times (proptest uses random inputs) and keep any new `proptest-regressions` files.
- Keep commits focused, with a short imperative subject ("Add ...", "Fix ...") and a body that explains why.

## Reporting problems

Please include the Loom version or commit, the provider and model, whether the session was in Observe, Suggest or Enforce mode, and the relevant log lines (`RUST_LOG=loom_server=debug`). Do not paste prompts or tool output you would not want public: the store holds them in plain text.

## License

By contributing you agree that your contributions are licensed under the Apache License 2.0.
