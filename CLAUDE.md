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
- Plans describe open decisions and actionable work. Remove completed handoffs
  and checkpoint history; Git retains them.
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
| `exomonad-worktree` | Managed coding checkouts, repository observation, and journal |
| `tidepool` | Public facade and composition-root binaries |

The harness runtime and browser assets come from the pinned external
`exomonad-harness` source; `Cargo.toml` and `flake.nix` own those dependencies.

Small support crates have short local charters describing their exact scope.

## Build and test

The `justfile` forwards to declared native Buck actions using configured,
materialized Nix tools. See `docs/swarm-builds.md` for server admission and
`AGENTS.md` for ownership and evidence rules.

```bash
just quick
just check                          # compile-only, no test execution
just test-target PACKAGE TARGET --exact FULL_NAME --expected-count 1
just test-lib PACKAGE --exact FULL_NAME --expected-count 1
just suite PACKAGE
just fixtures-check containers-contract
```

Native Rust runners discover actual libtest names, reject empty selections,
check counts and isolate each case. The 26 Cabal-declared Haskell suites use
one shared Tasty runner and a native component graph projected by the pinned
Cabal metadata producer. Compile-only binaries and ignored command adapters
do not establish passing test evidence. Generated immutable fixtures are runtime resources;
rebuild their owning source actions instead of updating checked-in blobs.

`just verify` is the broad native integration gate, reserved for integration
boundaries. Target availability and successful source generation are not
migration acceptance. Exact frozen bundle build/source/profile/compiler/asset
identity and production acceptance belong to `build/package/qualification.py`;
see `build/package/README.md`. Run/init frontends require explicit bundle,
descriptor and report paths.

## Architectural invariants

The public eval API is generated into the MCP tool description. Do not copy
that live reference into standing documentation.
