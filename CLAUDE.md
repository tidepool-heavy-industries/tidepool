# Tidepool

Tidepool compiles Haskell effect programs into Cranelift-backed state machines
driven from Rust. Haskell describes the computation; Rust executes it and
services its effects.

## Repository rules

- Cross-crate mechanisms have one implementation. Check the mechanism index
  before adding a cache, registry, path resolver, process launcher, durable
  log, identifier issuer, or supervision layer. Extend the existing mechanism
  instead of copying it.
- Root decisions govern cross-crate architecture. A crate's `CLAUDE.md` governs
  its local boundaries and invariants.
- `plans/README.md` lists active design work. Plans are temporary and are not
  standing architecture.
- Use the vocabulary in `docs/GLOSSARY.md`, especially in model-facing text.
- Keep history in git. Standing documentation describes the current system,
  not the sequence of changes that produced it.

## Workspace map

| Area | Responsibility |
|---|---|
| `haskell/` | GHC Core extractor and the Haskell stdlib |
| `tidepool-repr` | Core IR, constructor metadata, CBOR, shared identifiers |
| `tidepool-eval` | Tree-walking reference interpreter |
| `tidepool-heap` | JIT heap layout and copying-GC primitives |
| `tidepool-codegen` | Cranelift compiler and effect machine |
| `tidepool-toolchain` | Toolchain discovery, fingerprints, paths, and compile cache |
| `tidepool-runtime` | High-level compile/run API and machine-session substrate |
| `tidepool-protocol` | Source schema for effect and error definitions |
| `tidepool-mcp` | MCP server library and generated Haskell effect surface |
| `tidepool-handlers` | Concrete effect handlers |
| `tidepool-repl` | Stateful GHCi-style MCP server |
| `tidepool-harness` | Resident authored-harness runtime and driver |
| `tidepool-agent` | Typed coding-agent backend boundary |
| `tidepool-worktree` | Managed worktrees, repository observation, and journal |
| `tidepool-web` | Operator UI |
| `tidepool` | Public facade and composition-root binaries |
| `harness-dogfooding/` | Authored harnesses, including the Haskell devswarm |

Small support crates have short local charters describing their exact scope.

## Mechanism index

| Mechanism | Home |
|---|---|
| git subprocess invocation | `tidepool-worktree::git::GitCli` |
| durable JSONL append/read | shared primitive in `tidepool-repr` |
| config, cache, and project paths | `tidepool-toolchain::paths` |
| extractor CLI, typed requests, daemon, and worker invocation | `tidepool-extract-cmd` |
| compiled-artifact cache | `tidepool-toolchain::cache` |
| toolchain fingerprint and deploy handshake | `tidepool-toolchain::toolchain` |
| monotonic process-local identifiers | issuer in `tidepool-repr` |
| Haskell turn-module templates | `tidepool-runtime::session::turn` |
| durable-format migration ladders | `tidepool_repr::version_ladder` |
| turn timeout, cancellation, and crash supervision | `tidepool-runtime::TurnSupervisor` |
| machine-session checkout and ownership | `tidepool_runtime::session::registry` |
| MCP transport and resource catalog | helpers in `tidepool-mcp` |
| heap-to-`Value` decoding | `tidepool-codegen::heap_bridge` |
| Core free-variable analysis | `tidepool-repr::free_vars` |
| field/laziness triviality policy | `tidepool-repr` |
| authored Haskell concurrency | `Tidepool.Async` |
| operator forms, gates, and steering | `OperatorGate::present_form` and `Tidepool.Form` |
| operator listen queue and socket | `tidepool-harness::listen` |
| effect and error definitions | `tidepool-protocol` and unmigrated definitions in `tidepool-mcp/src/effect_defs.rs` |

## Build and test

```bash
nix develop
cargo check --workspace
cargo nextest run
cargo clippy --workspace
cargo fmt --all -- --check
```

`cargo nextest run` is the quick tier. The default filter skips GHC-heavy test
processes, although macro expansion can still invoke the extractor during a
fresh build.

For a targeted GHC-heavy test, use:

```bash
scripts/battery.sh -p <crate> -E 'test(<name>)'
```

For a full GHC-heavy crate, use `scripts/battery-shard.sh <crate>`. Some large
crates require the binary sub-shards listed in that script. Expensive ignored
tests additionally require `TIDEPOOL_EXPENSIVE_TESTS=1`.

Do not run an unfiltered workspace battery expecting it to complete in a
short-lived environment. Do not add a separate extractor compile when an
existing family bundle can carry another assertion.

After changing `haskell/`, follow `haskell/CLAUDE.md` to rebuild fixtures or
deploy the extractor and stdlib.

## Architectural invariants

- `CoreExpr` is `RecursiveTree<CoreFrame>`. Its principal frames are `Var`,
  `Lit`, `App`, `Lam`, `LetNonRec`, `LetRec`, `Case`, `Con`, `Join`, `Jump`, and
  `PrimOp`.
- Haskell serialization removes types, casts, and ticks before Rust reads the
  CBOR representation.
- The JIT heap uses a manual object layout and a copying collector. It is not a
  Rust enum graph.
- Freer continuations retain the `Leaf`/`Node` type-aligned sequence shape;
  they are not represented as a single closure.
- Union tags are unboxed word indices into the effect list.

The public eval API is generated into the MCP tool description. Do not copy
that live reference into standing documentation.
