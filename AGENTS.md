# Repository Guidelines

## Project Structure & Module Organization
- `src/main.rs`: Axum HTTP server entrypoint (`/health`, `/v1/chat/completions`, `/v1/messages`, `/v1/responses`).
- `src/lib.rs`: Crate exports.
- `src/protocol/`: Client/target protocol adapters and detector (OpenAI, Anthropic).
- `src/proxy/`: Upstream forwarding and streaming transport.
- `src/router/`: Business API routing and cache integration.
- `src/config/`: Typed config + loader (env overrides with prefix `GATEWAY__`).
- `src/cache/`, `src/telemetry/`, `src/models/`, `src/usage_collector.rs`: Cache, metrics/events, domain models, streaming usage.
- `docs/`: Reference docs (see `docs/architecture.md`).
- `config.yaml`: Runtime configuration. `Cargo.toml`/`Cargo.lock`: Rust metadata.

## Build, Test, and Development Commands
- Build: `cargo build` (use `--release` for optimized binary).
- Run: `cargo run` (reads `config.yaml`, binds to `server.host:server.port`).
- Lint/Format: `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all`.
- Test: `cargo test` (unit/integration tests; none committed yet).

Example request:
```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer <USER_TOKEN>' \
  -d '{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}'
```

## Coding Style & Naming Conventions
- Rust 2021 edition, 4‑space indent, `rustfmt` enforced. Keep `clippy` clean.
- Naming: `snake_case` for files/modules/functions, `CamelCase` for types, `SCREAMING_SNAKE_CASE` for consts.
- Errors: prefer crate `Result`/`Error` (`src/error`), use `thiserror` for typed errors.
- Modules: keep adapters/routers small and cohesive; avoid cross‑module leakage.

## Testing Guidelines
- Unit tests inline with modules: `#[cfg(test)] mod tests { ... }`.
- Integration tests in `tests/` using `reqwest`, `tower-test`, or `wiremock` as needed.
- Aim for coverage on routing decisions, protocol transforms, and proxy error mapping.

## Commit & Pull Request Guidelines
- Commit style follows Conventional Commits seen here: `feat:`, `fix:`, `refactor:`, etc.
- PRs should include: purpose, linked issues, config changes (`config.yaml`/env), affected endpoints, and brief test notes. Add screenshots/log snippets when relevant.

## Security & Configuration Tips
- Never log full secrets; mask tokens (only prefix/suffix). Avoid committing real keys.
- Config is file‑based with env overrides: e.g. `GATEWAY__SERVER__PORT=8081` or `GATEWAY__BUSINESS_API__BASE_URL=...`.
- Keep timeouts/retries reasonable to protect upstreams; validate inputs before forwarding.
