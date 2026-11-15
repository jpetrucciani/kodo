# CLAUDE.md — kodo

kodo is a Rust WoW addon manager (iced GUI + CLI) targeting WoW: Forever first.
Read SPEC.md before any change. Work milestone by milestone (M0 → M4); don't start
the next milestone until the current one's exit criteria pass on all three OSes in CI.

## Rules

- Workspace crates: kodo-core (no UI or HTTP deps; owns the `Source` trait and the
  `Engine`), kodo-sources (adapters), kodo-app (iced), kodo-cli. Frontends do everything
  through `kodo_core::engine::Engine`.
- Pin iced in [workspace.dependencies]; never bump it as a side effect of other work.
- No unwrap/expect/panic outside tests (enforced by workspace clippy lints).
  thiserror in libs, anyhow only in binaries.
- Every network call has a timeout and goes through kodo-sources.
- All colors, spacing, radii and fonts come from kodo-app/src/theme.rs tokens.
- Never write inside a WoW folder except through the install engine's atomic swap.
- Per-flavor facts (folder, product code, TOC suffix order, interface range) live only in
  the table in kodo-core/src/flavor.rs.
- No live network in tests; use fixtures in tests/fixtures and wiremock.
- Anything the spec marks "verify": check live docs or a real install first, and record
  the finding in SPEC.md under Open questions before coding against it.

## Commands

- nix-shell (or direnv): toolchain pinned by rust-toolchain.toml, plus nextest, cargo-deny, actionlint
- cargo nextest run --workspace
- cargo clippy --workspace --all-targets -- -D warnings
- cargo deny check
- cargo run -p kodo-app
- cargo run -p kodo-cli -- clients
- cargo run -p kodo-cli -- scan --path <WoW folder>
