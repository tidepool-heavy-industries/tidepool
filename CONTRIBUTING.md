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

Always verify your changes by running the workspace-wide tests and checks:

```bash
cargo check --workspace   # Type check the entire workspace
scripts/battery.sh        # Run ALL tests (cargo-nextest; builds TIDEPOOL_EXTRACT if unset)
cargo nextest run         # Quick tier: pure-Rust crates only
cargo clippy --workspace  # Run lints
```

The test runner is `cargo-nextest` (`cargo install cargo-nextest --locked`), which
runs each test in its own OS process. GHC-heavy crates need the `TIDEPOOL_EXTRACT`
env var pointing at a built `tidepool-extract-bin` — `scripts/battery.sh` sets this
up automatically. See the Build & Test section of `CLAUDE.md` for the full matrix.

## MCP Server

The `tidepool` binary is an MCP server. To build and run it locally:

```bash
cargo install --path tidepool
tidepool # Communicates via JSON-RPC over stdio
```

## Adding New Effects

The effect stack derives from a single declaration: `tidepool-mcp/src/effect_defs.rs`.
Each effect is one `<effect>_effect_def!` block; two projections generate the
effect declaration builder and the Rust `<Eff>Req` enum + handler dispatch from it.
Add, remove, or reorder an effect by editing that file, then write the handler
method the dispatch arm calls — see `tidepool-mcp/CLAUDE.md` (how to add an effect)
and `tidepool-handlers/CLAUDE.md` (handler arms, `cx.respond*` variants).

## Adding Prelude Functions

When adding or modifying functions in `haskell/lib/Tidepool/Prelude.hs`, keep the following in mind:

- **Monomorphization**: Polymorphic base functions that use typeclass dictionaries often crash when JIT-compiled because error branches in dictionaries are eagerly evaluated.
- **Shadowing**: Shadow polymorphic base functions with monomorphic versions that use primops directly (e.g., use `rem` instead of the `Integral` typeclass version).
- **Monomorphic shadows over dictionaries**: `Tidepool.Prelude` exports monomorphic versions of dictionary-heavy functions like `sum`, `product`, `maximum`, and `minimum`; follow that pattern for new additions.

## Testing Approach

- **Rust Tests**: Use unit tests and integration tests in the `tests/` directory of each crate.
- **Haskell Integration Tests**: Add test cases to `haskell/test/Suite.hs`. These tests are compiled to CBOR fixtures and verified by integration tests in `tidepool-eval/tests/haskell_suite.rs`.
- **Property-Based Testing**: Use `proptest` for complex logic like the bridge conversion and the JIT machine state transitions.

## Code Style

- **Formatting**: Run `cargo fmt` before committing.
- **Safety**: Avoid `todo!()`, `unimplemented!()`, `panic!()`, or `unwrap()` in production code. Use `Result` and handle errors gracefully.
- **Documentation**: Use doc comments (`///`) for all public-facing types and functions.
- **Consistency**: Follow the established naming and architectural patterns in the codebase.
