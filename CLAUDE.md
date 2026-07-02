# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Tidepool

Compile freer-simple effect stacks into Cranelift-backed state machines drivable from Rust. Haskell expands, Rust collapses. The language boundary is the hylo boundary.

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
  mechanism, known repl-specific friction.
- `tidepool-lsp/CLAUDE.md` — the `tidepool-lsp-daemon` sidecar: socket
  resolution, name/path-only protocol design, known limits.

The live **eval API reference** (what eval users can call) is the MCP `eval` tool
description emitted by the server — not duplicated in these files (it drifts).

## Build & Test

```bash
nix develop                              # Enter dev shell (provides Rust + GHC 9.12)
cargo check --workspace                  # Type check
cargo test --workspace                   # Run all tests
cargo test -p tidepool-codegen           # Run tests for one crate
cargo test -p tidepool-eval -- test_name # Run a single test by name
cargo clippy --workspace                 # Lint
cargo fmt --all -- --check               # Format check
cargo install --path tidepool            # Install the MCP server binary (`tidepool`)
```

Changed `haskell/`? See `haskell/CLAUDE.md` for the rebuild + deploy steps.

`scripts/redeploy.sh` — deploy extract + both servers + cache clear; see `haskell/CLAUDE.md` for what each step does

---

## Eval Records API

Effect verbs return named records, not positional tuples. Use **record-dot syntax**
(`p.stdout`, `h.path`) — bare selectors (`stdout p`) are ambiguous when duplicate
field names exist across record types.

| Record | Fields | Access example |
|--------|--------|----------------|
| `Proc` | `exitCode :: Int`, `stdout`, `stderr :: Text` | `p <- run cmd; p.stdout` |
| `Hit`  | `path`, `text :: Text`, `line :: Int` | `h.path`, `h.line` |
| `Doc`  | `path`, `body :: Text` | `d.path`, `d.body` |

Key helpers: `ok :: Proc -> Bool` (true when `exitCode == 0`); `run :: Text -> M Proc`;
`grepGlob :: Text -> FilePath -> M [Hit]`; `readGlob :: Text -> M [Doc]`.
`tryRun :: Text -> M (Either Text Proc)` — `Left` only on spawn failure; non-zero exit
is `Right proc`, inspect `proc.exitCode`.

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
