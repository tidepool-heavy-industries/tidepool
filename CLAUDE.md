# Tidepool

Tidepool compiles Haskell effect programs into Cranelift-backed state machines
driven from Rust. Haskell describes the computation; Rust executes it and
services its effects.

`AGENTS.md` is the contributor guide: how to work here, the language boundary,
and where to start. This file is the short form, with the mechanism index.

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
- Primitives, not helpers. When a dogfooding model hand-builds something (an
  artifact collector, a review gate, a wave timer), the harness supplies the
  missing piece: an OID on every settled reply, a Git that resolves it from
  the parent's view, a worktree the parent can execute in, timestamps as
  data, types that default. The model writes its own `collectArtifacts` in
  three lines if it wants one. We do not ship `collectArtifacts`. Helpers
  belong in a project's `.exomonad`, written by the model, or in skills as
  worked examples.

## Workspace map

| Area | Responsibility |
|---|---|
| `bridge/haskell/` | GHC-to-prepared-STG compiler worker and the Haskell stdlib |
| `tidepool-repr` | Prepared execution schema, constructor metadata, CBOR, shared identifiers |
| `tidepool-heap` | JIT heap layout and copying-GC primitives |
| `tidepool-codegen` | Cranelift compiler and effect machine |
| `tidepool-toolchain` | Toolchain discovery, fingerprints, paths, and compile cache |
| `tidepool-runtime` | High-level compile/run API and machine-session substrate |
| `tidepool-protocol` | Source schema for effect and error definitions |
| `tidepool-mcp` | MCP server library and generated Haskell effect surface |
| `tidepool-handlers` | Concrete effect handlers |
| `exomonad-agent` | Typed coding-agent backend boundary |
| `exomonad-worktree` | Managed coding checkouts, repository observation, and journal |
| `tidepool` | Public facade and composition-root binaries |
| `exomonad/harness-dogfooding/` | Authored harnesses, including the Haskell devswarm |

The retained `exomonad/harness/` and `exomonad/web/` source trees are historical
reference material and are excluded from the supported Cargo workspace.

Small support crates have short local charters describing their exact scope.

## Mechanism index

| Mechanism | Home |
|---|---|
| git subprocess invocation | `exomonad-worktree::git::GitCli` |
| interactive actor process mount boundary | `exomonad-node::process_boundary` |
| durable JSONL append/read | shared primitive in `tidepool-repr` |
| durable single-consumer delivery queue and ack cursor | `exomonad-node::DurableInbox` |
| config, cache, and project paths | `tidepool-toolchain::paths` |
| extractor CLI, typed requests, daemon, and worker invocation | `tidepool-extract-cmd` |
| compiled-artifact cache | `tidepool-toolchain::cache` |
| toolchain fingerprint and deploy handshake | `tidepool-toolchain::toolchain` |
| monotonic process-local identifiers | issuer in `tidepool-repr` |
| provider-neutral conversation values and call seam | `exomonad-model` |
| model-authored fenced-output parsing | `exomonad-model-output` |
| actor identity, lifecycle, turns, and events | `exomonad-actor` |
| Haskell turn-module templates | `tidepool-runtime::session::turn` |
| resident Haskell workbench sequencing and source classification | `tidepool-runtime::session::workbench` |
| durable-format migration ladders | `tidepool_repr::version_ladder` |
| turn timeout, cancellation, and crash supervision | `tidepool-runtime::TurnSupervisor` |
| machine-session checkout and ownership | `tidepool_runtime::session::registry` |
| MCP transport and resource catalog | helpers in `tidepool-mcp` |
| prepared-heap observation | `tidepool-codegen::prepared_program::observe` |
| prepared-program free-variable analysis | `tidepool-repr::free_vars` |
| field/laziness triviality policy | `tidepool-repr` |
| authored Haskell concurrency | `Tidepool.Async` |
| operator forms, gates, and steering | `OperatorGate::present_form` and `Tidepool.Form` |
| agent spec discovery, reload, and the after-tool slot | `exomonad-actor::{agent_spec, reload_spec_tool, after_tool}` |
| declared tool surface comparison | `exomonad-tool::surface` |
| source layers: capture, typecheck, publication, drift | `tidepool::exomonad::source` |
| Jev operators | pinned `jev-dsl` flake input; never vendored |
| effect and error definitions | `tidepool-protocol` and unmigrated definitions in `bridge/mcp/src/effect_defs.rs` |

## Build and test

```bash
just quick
just check
just test-target tidepool-runtime session 'test(<name>)'
just test-lib tidepool-runtime 'test(<name>)'
just suite tidepool-runtime
just changed
just verify
```

Large integration suites use small entry points in `tests/suites/` that import
separate test files as modules. Cargo's `autotests = false` prevents linking a
runtime copy for every file; `just suite-check` checks suite registration and
coverage of every top-level test file. Add new tests to the appropriate
suite entry point. Nextest still runs each test in its own process.

Use `just test-target CRATE SUITE 'test(module::name)'` to restrict compilation
as well as execution. `just test CRATE FILTER` selects tests across all targets
and may compile more than needed. `just suite CRATE` builds each declared suite
as it reaches it. Routine dev/test builds omit debug information while retaining
symbols and GC-required frame pointers. To opt into debugger information, set
`CARGO_PROFILE_DEV_DEBUG=2 CARGO_PROFILE_DEV_STRIP=none` (use `TEST` for tests).

The Justfile is the development entry point and enters the Nix shell itself.
`just --list` describes every supported workflow. `just quick` runs workspace
library tests under nextest process isolation. `just check` adds formatting,
strict clippy, and nextest's broader default-filter tier; macro expansion may
still invoke the extractor on a fresh build.

`just test` accepts an ordinary nextest filter expression. `just suite` runs
Cargo integration targets sequentially with one shared compile daemon. `just changed` is a conservative inner-loop selection, not the
pre-review gate; `just verify` is the gate.

Do not add a separate extractor compile when an existing family bundle can
carry another assertion. Expensive and known-bug ignored tests remain explicit
opt-ins rather than part of `just verify`.

After changing extractor translation or serialization, run
`just fixtures-check` or `just fixtures-update`. Follow `bridge/haskell/CLAUDE.md`
for deployment of the extractor and standard library.

## Architectural invariants

The public eval API is generated into the MCP tool description. Do not copy
that live reference into standing documentation.
