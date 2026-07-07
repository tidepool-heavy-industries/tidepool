# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Tidepool

Compile freer-simple effect stacks into Cranelift-backed state machines drivable from Rust. Haskell expands, Rust collapses. The language boundary is the hylo boundary.

**The core idea — bash++ for LLM agents.** Models are near-natively fluent in
Haskell from decades of training data, the same way they are in bash. Tidepool
presents a "basically GHCi" surface (one-shot `eval` + stateful repl over typed
effects) that inherits that fluency: one eval replaces N tool calls. Two rules
govern all surface work: (1) **the API is the prompt** — mirror canonical
Haskell/GHCi; every deviation is a fluency tax; (2) **the interface evolves as
an optimization loop** — clean-context model usage (wins vs bash, frictions)
drives UX changes and new effects, not speculative design.

---

## Rules

### Locked Decisions

The Key Decisions Reference section below is the source of truth for all architectural decisions. Every entry is final. Do not deviate from locked decisions. Do not re-derive them. If you need a decision that isn't there, escalate to the human.

### Plans

`plans/README.md` tracks the current active plan. Read it before starting new work.

> The agent-swarm orchestration protocol (roles, spawn tools, branch hierarchy)
> is **not** here — it lives in the devswarm role context
> (`~/.exo/roles/devswarm/context/root.md`), loaded each session. This file is
> codebase truth; that file is process truth.

---

## Project Structure

```
tidepool/
├── tidepool/              ← Facade crate + MCP server binary (`cargo install tidepool`)
├── tidepool-repr/         ← Core IR types: CoreExpr, DataConTable, CBOR serial  [CLAUDE.md]
├── tidepool-eval/         ← Tree-walking interpreter (oracle): Value, Env, lazy eval  [CLAUDE.md]
├── tidepool-heap/         ← Manual heap + copying GC for JIT runtime
├── tidepool-bignum/       ← Native ghc-bignum shims (Integer arith without GMP)
├── tidepool-optimize/     ← Optimization passes: beta, DCE, inline, case reduce
├── tidepool-bridge/       ← FromCore/ToCore traits + derive macros
├── tidepool-bridge-derive/← Proc-macro for bridge derives
├── tidepool-bridge-effects/← Single-source bridged-record types shared by handlers + test mocks
├── tidepool-macro/        ← Proc-macro for effect stack declarations
├── tidepool-effect/       ← Effect handling: DispatchEffect, EffectHandler, HList
├── tidepool-codegen/      ← Cranelift JIT compiler + effect machine  [CLAUDE.md]
├── tidepool-runtime/      ← High-level API: compile_haskell, compile_and_run, cache
├── tidepool-mcp/          ← MCP server library (generic over effect handlers)  [CLAUDE.md]
├── tidepool-handlers/     ← Central effect-request handler arms (`<Eff>Req` matches)  [CLAUDE.md]
├── tidepool-repl/         ← GHCi-style resident-session MCP server  [CLAUDE.md]
├── tidepool-lsp/          ← LSP client + workspace daemon (call graph, hover, refs)  [CLAUDE.md]
├── tidepool-testing/      ← Test utilities + property-based generators (internal)
├── examples/{guess,tide}/ ← Demos: number-guessing game, REPL
├── haskell/               ← Haskell harness (tidepool-extract) + test suite + stdlib  [CLAUDE.md]
│   └── lib/Tidepool/      ← Haskell stdlib (auto-imported in MCP)
├── flake.nix              ← Dev shell (Rust + GHC 9.12 with fat interfaces)
└── CLAUDE.md              ← YOU ARE HERE
```

**Per-crate `CLAUDE.md` files hold the crate-specific docs** (loaded when you work
in that directory):
- `haskell/CLAUDE.md` — rebuilding the toolchain, regenerating fixtures,
  extract diagnostics, the eval stdlib map + structured Ask/Llm surface, Known
  Limits, adding Prelude functions.
- `tidepool-repr/CLAUDE.md` — the self-rolled flat-vector `RecursiveTree` scheme,
  `DataConTable` hygiene (`insert_checked`, sibling-group disambiguation),
  session-id newtypes, CBOR wire-format versioning.
- `tidepool-eval/CLAUDE.md` — the JIT's differential oracle: trampoline
  join-point evaluation, WHNF-only `Value`, thunk lifecycle, how it's actually
  tested (differential harnesses, not its own unit suite).
- `tidepool-codegen/CLAUDE.md` — JIT/effect/cache diagnostics, case-trap → `runtime_case_trap` (poison + breadcrumb, not SIGILL).
- `tidepool-mcp/CLAUDE.md` — eval-authoring patterns (aperture/census/diff verbs),
  structural search, how to add an effect.
- `tidepool-handlers/CLAUDE.md` — the Rust side of the effect contract: adding a
  handler arm, the four `cx.respond*` variants, sandbox enforcement.
- `tidepool-repl/CLAUDE.md` — resident-session block-runner (decl/stmt/meta item
  classification), the single-owned `SessionState` lifecycle machine, ask/suspend
  mechanism, repl-specific usage notes.
- `tidepool-lsp/CLAUDE.md` — the `tidepool-lsp-daemon` sidecar: socket
  resolution, name/path-only protocol design, op-surface boundaries.

The live **eval API reference** (what eval users can call) is the MCP `eval` tool
description emitted by the server — not duplicated in these files (it drifts).

## Build & Test

```bash
nix develop                              # Enter dev shell (provides Rust + GHC 9.12)
cargo check --workspace                  # Type check
scripts/battery.sh                       # Run ALL tests incl. GHC-heavy crates (cargo-nextest; builds TIDEPOOL_EXTRACT if unset)
cargo nextest run                        # Quick tier: pure-Rust crates only (GHC-extract crates skipped by default-filter)
cargo nextest run -p tidepool-codegen    # Run tests for one pure-Rust crate
cargo nextest run --ignore-default-filter -p tidepool-runtime -E 'test(test_name)'  # A GHC-heavy crate needs --ignore-default-filter
cargo clippy --workspace                 # Lint
cargo fmt --all -- --check               # Format check
cargo install --path tidepool            # Install the MCP server binary (`tidepool`)
```

**Test runner is `cargo-nextest`** (`cargo install cargo-nextest --locked` if not
already on PATH), not plain `cargo test`. nextest runs every test in its own OS
process — never two tests sharing one — which structurally de-races the JIT's
process-global-ish state (signal handlers, GC, fork-safety harnesses) that the
old blanket `-- --test-threads=1` discipline used to serialize against by
brute force. See `.config/nextest.toml` for the hazard-audit note (which
historical hazards existed, why process-per-test resolves them, and why no
`test-group` overrides are needed today) and `scripts/battery.sh` for the
canonical full-suite invocation. Plain `cargo test --workspace -- --test-threads=1`
still works as a fallback (e.g. no `cargo-nextest` available) but is
noticeably slower — see the branch history for a measured comparison.

**Every test run needs `TIDEPOOL_EXTRACT`** pointing at a built
`tidepool-extract-bin`, or tests fail loud with `Metadata entry must be an
array of exactly 7`:
```bash
export PATH=<nix-ghc-with-packages>/bin:$PATH   # lens on the GHC package DB
cd haskell && cabal build tidepool-extract-bin
export TIDEPOOL_EXTRACT=$(cabal list-bin tidepool-extract-bin)
```
`scripts/battery.sh` does this automatically when `TIDEPOOL_EXTRACT` is unset.

Changed `haskell/`? See `haskell/CLAUDE.md` for the rebuild + deploy steps.

`scripts/redeploy.sh` — deploy extract + both servers + cache clear; see `haskell/CLAUDE.md` for what each step does

---

## Eval Records API

Effect verbs return named records, not positional tuples. Use **record-dot syntax**
(`p.stdout`, `h.path`) — bare selectors (`stdout p`) are ambiguous when duplicate
field names exist across record types.

| Record | Fields | Access example |
|--------|--------|----------------|
| `Proc`        | `exitCode :: Int`, `stdout`, `stderr :: Text` | `Right p <- run cmd; p.stdout` |
| `Hit`         | `path`, `text :: Text`, `line :: Int` | `h.path`, `h.line` |
| `FileRead`    | `path :: Text`, `contents :: Either FsError Text` | `r.path`, `r.contents` |
| `Commit`      | `sha`, `subject`, `author`, `date`, `files :: [Text]` | `Right c <- gitShow "HEAD"; c.sha`, `c.files` |
| `StatusEntry` | `path`, `state :: Text` | `Right es <- gitStatus; map (.state) es` |
| `FileDelta`   | `path :: Text`, `adds`, `dels :: Int`, `binary :: Bool` | `Right ds <- gitDiffStat "HEAD~1"; ds` |

Key helpers: `ok :: Proc -> Bool` (true when `exitCode == 0`);
`run :: Text -> M (Either ExecError Proc)` — spawn/bad-dir failure is TYPED (#335);
a nonzero exit is still `Right proc`, inspect `proc.exitCode`. Natural spelling:
`Right p <- run cmd` (or `run cmd >>= liftEither`).
`grepGlob :: Text -> FilePath -> M (Either FsError [Hit])`;
`readGlob :: Text -> M [FileRead]` — per-file failure isolation, not a verb-level Either.

---

## Key Decisions Reference

Critical architectural decisions for daily work (the Locked Decisions source of truth):

- **CoreFrame variants:** Var, Lit, App, Lam, LetNonRec, LetRec, Case, Con, Join, Jump, PrimOp
- **No type variants** — types stripped at serialization in Haskell
- **RecursiveTree\<CoreFrame\>** as CoreExpr type alias
- **CBOR** via serialise (Haskell) / ciborium (Rust)
- **Cast/Tick/Type erasure** happens in Haskell serializer, NOT in Rust
- **HeapObject:** manual memory layout (raw byte buffers + unsafe accessors), NOT a Rust enum
- **GC:** Copying collector, custom RBP frame walker, split gc-trace/gc-compact
- **freer-simple continuations:** Leaf/Node tree (type-aligned sequence), NOT single closures
- **Union tags:** unboxed Word# constants (0##, 1##, ...) indexing the effect type list
