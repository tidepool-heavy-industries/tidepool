# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Tidepool

Compile freer-simple effect stacks into Cranelift-backed state machines drivable from Rust. Haskell expands, Rust collapses: the hylomorphism's unfold/fold split falls exactly on the language boundary.

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

### Authority

Root CLAUDE.md's Key Decisions Reference is supreme for cross-crate
architecture. Per-crate CLAUDE.md files are authoritative for crate-local
contracts, and subordinate to root on any conflict. `plans/` and PRD
"locked"/"frozen" language is a design-time record, not standing authority —
standing authority always runs through the CLAUDE.md hierarchy.

### One mechanism, one home

Every general-purpose mechanism (durable logs, retries, caches, registries,
template builders, path resolution, process spawning, id minting, supervision)
has exactly ONE implementation, listed in the Mechanism Index below. Before
building anything of that shape, check the index and grep — the capability may
exist under another plane's vocabulary. Three hard rules:

- **A file boundary is never license to copy.** If the mechanism you need
  lives in code you may not touch, STOP and escalate; a local reimplementation
  is the one forbidden resolution.
- **Kept-in-sync copies are forbidden.** A "must stay identical to X" comment
  or a byte-identity cross-test between two implementations is a bug report,
  not a maintenance strategy — extract the shared thing or escalate.
- **An API one notch too narrow is extended, not shadowed.** If the canonical
  home answers the wrong granularity (root-only where you need per-node),
  widen it in place.

### Plans

`plans/README.md` tracks the current active plan. Read it before starting new work.

> The agent-swarm orchestration protocol (roles, spawn tools, branch hierarchy)
> is **not** here — it lives in the devswarm role context
> (`~/.exo/roles/devswarm/context/root.md`), loaded each session. This file is
> codebase truth; that file is process truth.

### Terminology

`docs/GLOSSARY.md` is the naming authority. Compositions of well-known
industry terms beat coinage — a new term survives only when no reasonable
composition of standard terms carries the distinction (and then it goes IN
the glossary). Bare "window" is banned: **context window** (provider token
limit) is the only surviving use; the execution units are **model round**,
**agent session**, **loop iteration**. "Session" is always qualified
(agent/machine). Model-facing prompt text follows the glossary's prompt
rules — invented jargon there is a fluency tax on the models themselves.
Old spellings migrate on contact; identifier and serialized-field renames
are staged deliberately, never drive-by.

### Doc history

Inline doc-history ("an earlier version…", "this used to…", superseded
rationale, wave/lane narration) is DELETED, not archived — git is the store.
`plans/decision-archive/` is reserved for the narrow case where losing the
backstory invites re-tripping a hazard (a fix silently re-derived because
nobody wrote down that it was already made); those get a one-line pointer at
the site, never inline prose. Never applies to the Key Decisions Reference
below — it stays here verbatim.

---

## Project Structure

```
tidepool/
├── tidepool/              ← Facade crate + MCP server binary (`cargo install tidepool`); also the composition-root `tidepool-selfharness` binary (driver + gate + provider + memory store + web server)
├── tidepool-repr/         ← Core IR types: CoreExpr, DataConTable, CBOR serial  [CLAUDE.md]
├── tidepool-eval/         ← Tree-walking interpreter (oracle): Value, Env, lazy eval  [CLAUDE.md]
├── tidepool-heap/         ← Manual heap + copying GC for JIT runtime
├── tidepool-bignum/       ← Native ghc-bignum shims (Integer arith without GMP)
├── tidepool-optimize/     ← Optimization passes: beta, DCE, inline, case reduce
├── tidepool-bridge/       ← FromCore/ToCore traits + derive macros
├── tidepool-bridge-derive/← Proc-macro for bridge derives
├── tidepool-bridge-effects/← Single-source bridged-record types shared by handlers + test mocks
├── tidepool-macro/        ← Proc-macros embedding Haskell source as CBOR at build time (haskell_eval!/haskell_inline!)
├── tidepool-extract-cmd/  ← The ONE `tidepool-extract` invocation builder: bin resolution, typed args, the spawn + spawn counter. std-only leaf (zero deps)
├── tidepool-effect/       ← Effect handling: DispatchEffect, EffectHandler, HList
├── tidepool-agent/        ← Typed headless subagents: the backend seam + the Codex adapter (the ONLY place a coding backend is named)  [CLAUDE.md]
├── tidepool-codegen/      ← Cranelift JIT compiler + effect machine  [CLAUDE.md]
├── tidepool-runtime/      ← High-level API: compile_haskell, compile_and_run, cache
├── tidepool-mcp/          ← MCP server library (generic over effect handlers)  [CLAUDE.md]
├── tidepool-handlers/     ← Central effect-request handler arms (`<Eff>Req` matches)  [CLAUDE.md]
├── tidepool-repl/         ← GHCi-style resident-session MCP server  [CLAUDE.md]
├── tidepool-harness/      ← Resident harness: session-tree turn lifecycle, SessionRegistry checkout ownership, selfharness driver  [CLAUDE.md]
├── tidepool-worktree/     ← Managed worktrees, durable registry, typed repository events (PRD 19)  [CLAUDE.md]
├── tidepool-web/          ← Web operator GUI: AskUser form rendering + the operator gate  [CLAUDE.md]
├── tidepool-testing/      ← Test utilities + property-based generators (internal)
├── examples/{guess,tide}/ ← Demos: number-guessing game, REPL
├── harness-dogfooding/    ← Authored harnesses (companion, recursive-companion, dev-tree) run by the selfharness driver
├── haskell/               ← Haskell harness (tidepool-extract) + test suite + stdlib  [CLAUDE.md]
│   └── lib/Tidepool/      ← Haskell stdlib (auto-imported in MCP)
├── flake.nix              ← Dev shell (Rust + GHC 9.12 with fat interfaces)
└── CLAUDE.md              ← YOU ARE HERE
```

## Mechanism Index

The one home for each cross-cutting mechanism (see "One mechanism, one home"
above). If a needed mechanism is missing from this list, add it WITH its home
in the same change that creates it.

| Mechanism | Home |
|---|---|
| git subprocess invocation | `tidepool-worktree::git::GitCli` (the ONE git call site) |
| durable JSONL append/read | the shared primitive under `tidepool-repr` (consumers: worktree journal, handlers journal, harness log, selfharness observer) |
| config/cache/project path resolution | `tidepool-runtime::paths` |
| executable resolution + strict validation | `tidepool-extract-cmd` |
| in-process monotonic id minting | `tidepool-repr`'s issuer |
| Haskell turn-module templates (bind/expr wrappers) | `tidepool-runtime::session::turn` |
| turn thread supervision (timeout/cancel/crash) | `tidepool-runtime`'s `TurnSupervisor` |
| session checkout/ownership | `tidepool_runtime::session::registry` (`SessionRegistry`/`Slot`/`Checkout`/`SingleSlot` — `tidepool-harness`'s `registry.rs` and `tidepool-repl`'s `manager.rs` are thin clients) |
| MCP transport + resource catalog | `tidepool-mcp` shared helpers |
| heap → `Value` decoding | `tidepool-codegen::heap_bridge` |
| Core free-variable analysis | `tidepool-repr::free_vars` |
| field/laziness triviality policy | `tidepool-repr` (both JIT and eval call it) |
| concurrency for authored/model Haskell | `Tidepool.Async` (Control.Concurrent.Async mirror) |
| operator interaction (forms, gates, steering) | the ask/form machinery (`OperatorGate::present_form` + `Tidepool.Form`) — never a second channel |
| effect/error type definitions | the effect schema (`effect_defs.rs` / `tidepool-protocol`) — every projection generated, never hand-carried |

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
- `tidepool-codegen/CLAUDE.md` — JIT/effect/cache diagnostics, case-trap → `emit_case_trap` (poison + breadcrumb, not SIGILL).
- `tidepool-mcp/CLAUDE.md` — eval-authoring patterns (aperture/census/diff verbs),
  structural search, how to add an effect.
- `tidepool-handlers/CLAUDE.md` — the Rust side of the effect contract: adding a
  handler arm, the `cx.respond`/`respond_list` variants, sandbox enforcement.
- `tidepool-agent/CLAUDE.md` — the containment boundary, the resumable
  `AgentBackend` step seam (and why the tool-dispatch loop lives in Haskell),
  model-policy allowlists, and the mock-vs-recording-vs-live testing tiers.
- `tidepool-repl/CLAUDE.md` — resident-session block-runner (decl/stmt/meta item
  classification), the single-owned `SessionState` lifecycle machine, ask/suspend
  mechanism, repl-specific usage notes.
- `tidepool-harness/CLAUDE.md` — the resident harness: turn driving, session
  ownership (`checkout_run`/`run_checked_out` over `SessionRegistry`), the
  selfharness driver, hole cards.
- `tidepool-worktree/CLAUDE.md` — managed worktrees, the durable registry,
  the one `git` call site, and the rules PRD 19 draws around them.
- `tidepool-web/CLAUDE.md` — operator GUI rendering and the AskUser form wire
  shape.

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
scripts/bench-turn.sh                    # Turn-latency instrument: one table, cold/warm/session/block/harness rows (median-of-3)
```

**Test runner is `cargo-nextest`** (`cargo install cargo-nextest --locked` if not
already on PATH), not plain `cargo test`. nextest runs every test in its own OS
process — never two tests sharing one — which structurally de-races the JIT's
process-global-ish state (signal handlers, GC, fork-safety harnesses); that is
why the only `test-group` override needed is the GHC fan-out cap. See
`.config/nextest.toml` for the default-filter and `ghc-heavy` group cap
themselves (the case-by-case hazard audit that justified them lives in that
file's git history, not inline) and `scripts/battery.sh` for the canonical
full-suite invocation. Plain `cargo test --workspace -- --test-threads=1`
still works as a fallback (no `cargo-nextest` available) but is noticeably slower.

**Every test run needs `TIDEPOOL_EXTRACT`** pointing at a built
`tidepool-extract-bin`, or tests fail loud with `Metadata entry must be an
array of exactly 9` (a stale extract predating the current wire shape reports
the same message with its own old count — see `scripts/toolchain-doctor.sh`
for identifying that class):
```bash
export PATH=<nix-ghc-with-packages>/bin:$PATH   # lens on the GHC package DB
cd haskell && cabal build tidepool-extract-bin
export TIDEPOOL_EXTRACT=$(cabal list-bin tidepool-extract-bin)
```
`scripts/battery.sh` does this automatically when `TIDEPOOL_EXTRACT` is unset.

### Test tiers

This environment hard-kills background processes at ~380s, and a full-workspace
GHC battery is HOURS (every GHC-heavy test forks a real `tidepool-extract`
compile, capped at 2 concurrent per run, and a handful of suites alone run
for multiple hundreds of seconds).
Bare `scripts/battery.sh` WILL get killed mid-run. Four tiers, from fastest to
most exhaustive:

Both battery scripts take a host GHC slot for you (`scripts/ghc-slots.sh run`,
6 slots shared box-wide) and re-exec themselves under it — you do not wrap
them, and an outer wrapper is respected rather than double-acquired. Inside a
run, `.config/nextest.toml`'s `ghc-heavy` test group caps concurrent extract
compiles at 2; membership is default-deny, so a new crate or test binary is
capped without an edit there. The box-wide ceiling is the product of the two
(slots × per-run cap).

1. **Fast default** — `cargo nextest run`. This is a RUN-time selection
   filter, not a build-time GHC-free guarantee: `.config/nextest.toml`'s
   `default-filter` skips every GHC-extract-heavy crate's test PROCESSES, but
   `haskell_eval!`/`haskell_inline!` still run `tidepool-extract` during macro
   EXPANSION (i.e. at `rustc` build time) for any crate that uses them, so the
   first build after a toolchain bump or `.hs` edit still pays that cost —
   see `.config/nextest.toml`'s own header comment. This is the inner-loop
   tier; safe to run unattended once built.
2. **Targeted** — `scripts/battery.sh -p <crate> -E 'binary(<x>)'` (or
   `-E 'test(<name>)'`). One GHC-heavy crate, one test/binary, via
   `--ignore-default-filter`. The right tier for "does my change to crate X
   still pass".
3. **Sharded-full** — `scripts/battery-shard.sh <crate>`. Runs one entire
   GHC-heavy crate's tests (`--ignore-default-filter -p <crate>`). Only
   `tidepool-handlers` (186 tests) fits this whole within the ~380s budget as
   a bare `-p <crate>` invocation — `tidepool-harness`/`tidepool-runtime`/
   `tidepool-repl` each need `-E 'binary(...) or binary(...)'` sub-shards
   (7/7/7 respectively; the exact groups are documented in
   `scripts/battery-shard.sh`'s header — per-shard timings aren't tracked
   there). Chain shards (one invocation per
   group, in sequence — concurrent GHC-heavy shards on a shared box compete
   for the same `ghc-slots.sh` semaphore and inflate every number) to walk
   full coverage without tripping the environment's kill.
4. **Expensive, opt-in** — `TIDEPOOL_EXPENSIVE_TESTS=1 scripts/battery-shard.sh
   <crate>` (or targeted per-test). A handful of suites
   (`corpus_report`, `haskell_suite_differential`,
   `tidepool-testing::haskell_verified`,
   `tidepool-codegen::call_depth_sequential_vs_nested`'s
   `fifty_thousand_sequential_calls_do_not_false_positive_overflow` and
   `genuinely_deep_recursion_still_overflows_cleanly`) are dual-gated: each is
   `#[ignore]`d (so a default run reports them as ignored, never a silent
   pass) AND early-returns with a `SKIPPED (expensive)` line unless
   `TIDEPOOL_EXPENSIVE_TESTS=1` is set, even under `--run-ignored all` — this
   holds even under `--ignore-default-filter`, so tier 3 alone never
   accidentally triggers them. Run these deliberately, one at a time, outside
   the ~380s assumption: `corpus_report` and `haskell_suite_differential` are
   actually quick once gated in (measured ~8s and ~27s respectively), the
   call-depth pair takes roughly a minute and a half each, but
   `tidepool-testing::haskell_verified` genuinely runs for many hundreds of
   seconds — its proptest cases (e.g. `cousins::test_list_fold`) individually
   take 100s+.

Never run bare `scripts/battery.sh` (tier 0, unbounded) expecting it to
complete here — use tier 2 or 3.

**Suite wall time is a standing constraint — a new test must not pay its own
extract compile when a family bundle exists.** The idiom is one substrate,
many assertions: a new stdlib/JIT probe joins its family bundle's named-check
list (`jit_surface.rs`, the `stdlib_regressions` bundles, the handlers
per-effect families) instead of adding a `#[test]` that forks another
~5-8s compile; per-variant one-liners join exhaustive tables. Standalone
stays correct for: crash-class tests (a bundled crash destroys sibling
diagnosis), compile-fail assertions, sanctioned reds, property tests, and
distinct-fixture suites. A new per-test compile is not a regression per se —
but it has a real, permanent cost, so reviews should scrutinize each one:
does this test need its own compile, or does its assertion belong in a
family bundle?

**Compile compiles are memoized, and test processes SHARE the memo.**
`$TIDEPOOL_COMPILE_CACHE_DIR` (default: the cache dir) locates the
content-addressed compiled-artifact memo, and the harness suite points it at
the ambient cache dir while keeping each test's mutable state in its own
tempdir — so a second run of a GHC-heavy leg is much cheaper than the first
(measured on three harness binaries: 99s cold, 47s warm). Two consequences:
a COLD number needs the memo removed (`rm -rf $XDG_CACHE_HOME/tidepool`), and
a test that measures compile COST must pin its own memo dir rather than
inherit the shared one. See `plans/compile-memo.md`.

Changed `haskell/`? See `haskell/CLAUDE.md` for the rebuild + deploy steps.

`scripts/redeploy.sh` — deploy extract + both servers + cache clear + deploy stamp; see `haskell/CLAUDE.md` for what each step does, including the toolchain locator precedence tables and the deploy handshake

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
`readGlob :: Text -> M [FileRead]` — per-file failure isolation, not a verb-level Either;
a glob matching nothing yields `[]`, not an error.

---

## Key Decisions Reference

Critical architectural decisions for daily work (the Locked Decisions source of truth):

- **CoreFrame variants:** Var, Lit, App, Lam, LetNonRec, LetRec, Case, Con, Join, Jump, PrimOp
- **No type variants** — types stripped at serialization in Haskell
- **RecursiveTree\<CoreFrame\>** as CoreExpr type alias
- **CBOR** via serialise (Haskell) / ciborium (Rust)
- **Cast/Tick/Type erasure** happens in Haskell serializer, NOT in Rust
- **HeapObject:** manual memory layout (raw byte buffers + unsafe accessors), NOT a Rust enum
- **GC:** Copying collector (Cheney scan), custom RBP frame walker in tidepool-codegen; tidepool-heap provides shared object layout + `gc::raw` copy primitives
- **freer-simple continuations:** Leaf/Node tree (type-aligned sequence), NOT single closures
- **Union tags:** unboxed Word# constants (0##, 1##, ...) indexing the effect type list
