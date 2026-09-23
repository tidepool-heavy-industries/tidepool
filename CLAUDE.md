# Tidepool

Tidepool compiles Haskell effect programs into Cranelift-backed state machines
driven from Rust. Haskell describes the computation; Rust executes it and
services its effects.

`AGENTS.md` is the contributor guide: how to work here, the language boundary,
and where to start. This file is a short repository overview. Contributor rules
and the cross-crate ownership map live in `AGENTS.md`; crate boundaries live in
the nearest crate guide.

## Repository rules

- Cross-crate mechanisms have one implementation. Check the ownership map in
  `AGENTS.md` before adding a cache, registry, path resolver, process launcher,
  durable log, identifier issuer, or supervision layer. Extend the existing
  mechanism instead of copying it.
- Root decisions govern cross-crate architecture. A crate's `CLAUDE.md` governs
  its local boundaries and invariants.
- `plans/README.md` lists open design questions and retained evidence. Plans
  are temporary and are not
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

`just daemon-start` keeps one compile daemon warm across test runs; battery,
suite, and check runs reuse it automatically while its producer matches the
current extractor and worker; after a rebuild they start their own daemon
instead, so a restart only restores warmth. Other agents and test runs share
the persistent daemon: restart it once, at a quiet point, never per parcel.
Leave the host cargo config's incremental compilation on (no
`CARGO_INCREMENTAL=0`) and cap jobs to fit beside a 10 GiB GHC worker.

Large integration suites use small entry points in `tests/suites/` that import
separate test files as modules. Cargo's `autotests = false` prevents linking a
runtime copy for every file; `just suite-check` checks suite registration and
coverage of every top-level test file. Add new tests to the appropriate
suite entry point. Nextest still runs each test in its own process.

`just test-find NAME` prints the exact command for a test function without
building. Use `just test-target CRATE SUITE 'test(module::name)'` to restrict
compilation as well as execution. `just test CRATE FILTER` selects tests across all targets
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
