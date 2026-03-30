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
cargo test --workspace    # Run all tests
cargo clippy --workspace  # Run lints
```

## MCP Server

The `tidepool` binary is an MCP server. To build and run it locally:

```bash
cargo install --path tidepool
tidepool # Communicates via JSON-RPC over stdio
```

## Adding New Effects

Adding an effect involves changes in both Haskell and Rust:

1.  **Haskell**: Define your effect type and operations (e.g., in `haskell/lib/Tidepool/MyEffect.hs`).
2.  **Rust Request Type**: Define a Rust struct or enum that represents the effect request.
3.  **Bridge Implementation**: Use `#[derive(FromCore, ToCore)]` from `tidepool-bridge` to enable conversion between Haskell values and your Rust request type.
4.  **Effect Handler**: Implement the `EffectHandler` trait from `tidepool-effect` for your request type.
5.  **Dispatch**: Register your handler in the `HList` used by the `JitEffectMachine`.

## Adding Prelude Functions

When adding or modifying functions in `haskell/lib/Tidepool/Prelude.hs`, keep the following in mind:

- **Monomorphization**: Polymorphic base functions that use typeclass dictionaries often crash when JIT-compiled because error branches in dictionaries are eagerly evaluated.
- **Shadowing**: Shadow polymorphic base functions with monomorphic versions that use primops directly (e.g., use `rem` instead of the `Integral` typeclass version).
- **Avoid Dictionary-Heavy Functions**: Functions like `sum`, `product`, `maximum`, and `minimum` now work via lazy poison closures.

## Testing Approach

- **Rust Tests**: Use unit tests and integration tests in the `tests/` directory of each crate.
- **Haskell Integration Tests**: Add test cases to `haskell/test/Suite.hs`. These tests are compiled to CBOR fixtures and verified by integration tests in `tidepool-eval/tests/haskell_suite.rs`.
- **Property-Based Testing**: Use `proptest` for complex logic like the bridge conversion and the JIT machine state transitions.

## Code Style

- **Formatting**: Run `cargo fmt` before committing.
- **Safety**: Avoid `todo!()`, `unimplemented!()`, `panic!()`, or `unwrap()` in production code. Use `Result` and handle errors gracefully.
- **Documentation**: Use doc comments (`///`) for all public-facing types and functions.
- **Consistency**: Follow the established naming and architectural patterns in the codebase.
