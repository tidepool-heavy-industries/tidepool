# Contributing to Tidepool

Welcome to Tidepool! This guide will help you get started with contributing to the project.

## Prerequisites

- **Nix**: Required for the GHC toolchain (GHC 9.12 with fat interfaces).
- **Rust**: The core runtime and JIT compiler are written in Rust.

To enter the development environment, run:

```bash
nix develop
```

This will provide you with the correct versions of Rust, GHC, and other dependencies.

## Build and Test Commands

```bash
cargo check --workspace   # Type check the entire workspace
cargo nextest run         # Quick tier: pure-Rust crates only (the normal local command)
cargo clippy --workspace  # Run lints
```

For anything beyond the quick tier — GHC-heavy crates, targeted vs.
sharded-full vs. expensive test runs, and why bare `scripts/battery.sh`
should not be your default — see the **Test tiers** subsection of the root
`CLAUDE.md`'s Build & Test section. That table is the canonical matrix; it
is not duplicated here so the two cannot drift apart.

## MCP Server

The `tidepool` binary is an MCP server. To build and run it locally:

```bash
cargo install --path tidepool
tidepool # Communicates via JSON-RPC over stdio
```

## Adding New Effects

There are two paths today, depending on whether the effect you're touching
has migrated to the schema-driven scaffold yet
(`tidepool-protocol/src/effects/` — currently `Exec`, `Journal`, `Worktree`,
`RepoEvent`; check `tidepool_protocol::effects::all()` for the current set).

- **Schema-driven (migrated effects):** the effect's truth lives as data in
  `tidepool-protocol/src/effects/<effect>.rs` (a `schema::Effect` value), and
  `tidepool-protocol-gen` (the crate's `[[bin]]`) projects it into the macro
  DSL, wire mirrors, extractor verb tables, and harness classification lists
  that used to be hand-maintained separately. Edit the schema, regenerate,
  and the golden byte-compatibility check catches drift — see
  `tidepool-protocol/README.md`'s "How to change it" section for the
  migration procedure.
- **Legacy (everything else):** still declared by hand in
  `tidepool-mcp/src/effect_defs.rs`. Each effect is one `<effect>_effect_def!`
  block; two projections generate the effect declaration builder and the
  Rust `<Eff>Req` enum + handler dispatch from it. Add, remove, or reorder an
  effect by editing that file, then write the handler method the dispatch
  arm calls — see `tidepool-mcp/CLAUDE.md` (how to add an effect) and
  `tidepool-handlers/CLAUDE.md` (handler arms, `cx.respond*` variants).

New effects should generally target the schema-driven path going forward —
see the Effect Protocol PRD linked from `plans/README.md`.

## Adding Prelude Functions

When adding or modifying functions in `haskell/lib/Tidepool/Prelude.hs`, keep the following in mind:

- **Dictionary polymorphism runs on the JIT**: custom classes, multi-param classes, and GADT type-indexed dispatch all compile and execute — write the polymorphic version by default.
- **Surface shadows are the exception**: a few functions deliberately differ from base Prelude for runtime integration (for example `round`, and `show :: Render a => a -> Text`), not as a general pattern to follow. See `haskell/CLAUDE.md`'s "Adding new Prelude functions" section for the enforcement mechanism (`tidepool-runtime/tests/jit_surface.rs`).

## Testing Approach

- **Rust Tests**: Use unit tests and integration tests in the `tests/` directory of each crate.
- **Haskell Integration Tests**: Add test cases to `haskell/test/Suite.hs`. These tests are compiled to CBOR fixtures and verified by integration tests in `tidepool-eval/tests/haskell_suite.rs`.
- **Property-Based Testing**: Use `proptest` for complex logic like the bridge conversion and the JIT machine state transitions.

## Code Style

- **Formatting**: Run `cargo fmt` before committing.
- **Safety**: Avoid `todo!()`, `unimplemented!()`, `panic!()`, or `unwrap()` in production code. Use `Result` and handle errors gracefully.
- **Documentation**: Use doc comments (`///`) for all public-facing types and functions.
- **Consistency**: Follow the established naming and architectural patterns in the codebase.
