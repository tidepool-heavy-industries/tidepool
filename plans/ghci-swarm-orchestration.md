# GHCi-session — swarm/orchestration design (round-3 architecture)

Companion to `plans/ghci-session-persistence.md` (the what/why). This is the how:
the dependency DAG, wave structure, worktree/branch hierarchy, and per-subtask specs.
Supersedes the earlier 4-phase wave sketch (stale after round-3).

## Integration model

- **Integration branch:** `ghci-session` off `main`. All wave branches merge here; `main`
  merge only when the full MVP is green.
- **Worktrees only.** Each leaf works in its own `git worktree` under `/tmp/`. Never edit
  another agent's worktree, never checkout another branch. Branch naming:
  `ghci-session.<wave>-<slug>` (e.g. `ghci-session.w1-reentry`).
- **Governing idiom: scaffold → spawn → integrate (every leaf, parallel OR serial).** Default
  structure for *all* work here, not just the parallel waves (user, 2026-06-25). Even a serial
  dependency chain runs as: scaffold the contract → **spawn a subagent** for the unit →
  **adversarial review at its submit/branch-for-merge boundary** → integrate. The per-merge
  adversarial review is a first-class quality gate built into the spawn tooling, so spawning pays
  off even when a unit can't run in parallel. **Bias toward this idiom on the edge:** if something
  is borderline "just do it inline," prefer scaffolding + spawning so it gets the merge-boundary
  review. Serial units spawn *sequentially* (next spawns after the prior integrates); parallel
  units spawn together. Scaffold first so each spawn target is a clean, contract-bounded branch.
- **Wave protocol:** root freezes the wave's contracts → spawns the wave's leaves (parallel where
  files are disjoint, sequential where they share a seam) → each leaf builds/tests in its worktree
  → **adversarial review at each leaf's merge boundary** → root merges into `ghci-session` →
  **verify the build after each merge** → next wave.
- **Verify gate per leaf:** `cargo check --workspace` + the leaf's own tests + `cargo clippy`.
  Codegen leaves additionally run `cargo test -p tidepool-codegen`.
- **Testing principle (user, 2026-06-25) — acceptance tests drive the REAL entry point.** Every
  test that *claims a feature works* must exercise the actual ghci-style session entry point
  (`session_open`/`session_eval`/…) over **multiple real turns**, letting allocation/GC happen
  **organically** — the same path production uses. Bespoke low-level harnesses (manually wiring
  `add_function`/`run_fragment`, artificially forcing a GC) are allowed ONLY as unit smoke checks
  for internals, **never** as the proof of the feature: a synthetic path can pass while the real
  one is broken. The artificial-GC, hand-wired "proof test" is demoted to a Wave-1 smoke check; the
  acceptance proofs live at Wave 2 (multi-turn machine/heap persistence through the entry point) and
  Wave 3 (the headline `x <- … ; slice x` across turns, with enough turns that GC fires naturally).
- **tmp/ caveat:** another agent uses repo-root `tmp/` as scratch — never `git add -A`, stage
  specific paths; fetch-before-push; never force-push.

## Component inventory (round-3 punch-list → files)

| # | Component | Primary files | Crate |
|---|---|---|---|
| A | Decl/function/type-sig accumulation (session library) | `eval_prep.rs`, `tidepool-runtime/src/lib.rs`, `cache.rs` | mcp/runtime |
| B | Gen-suffix naming + one `g` counter + cache-key session id | session mgr (source-gen), `cache.rs`; (B2 reshape: naming input only) | mcp/runtime |
| C | Re-entry APIs: `add_function`/`run_fragment`, env-seeding, `from_external_pointer` | `jit_machine.rs`, `pipeline.rs`, `emit/expr.rs`, `emit/mod.rs` | codegen |
| D | `PERSISTENT_ROOTS` (run- vs session-scoped roots) | `host_fns.rs`, `jit_machine.rs` (`RegistryGuard`) | codegen |
| E | Old-gen tenuring (`old_space`, nursery-only minor GC) | `host_fns.rs` (perform_gc/GcState), `tidepool-heap/src/gc`, `nursery.rs` | codegen/heap |
| F | Persistent nursery bump-cursor | `nursery.rs`, `jit_machine.rs` | codegen |
| G | `SessionKind::Session` + resident worker + affinity | `ask.rs`, `server.rs` | mcp |
| H | GHC type capture (`idType` → render → `PipelineResult`) | `GhcPipeline.hs`, extract output, Rust read side | haskell/runtime |
| I | OPAQUE synthetic value-decl injection + capability override | `eval_prep.rs`, session mgr | mcp |
| J | Two-layer `name → valueId(0xFD) → root` binding table + `0xFD` resolution | session mgr, `emit/expr.rs` | mcp/codegen |
| K | Strict-force-at-bind (deep-force result before tenuring) | `jit_machine.rs` (uses existing iterative deep paths) | codegen |
| M | Regression tests: case-trap-graceful + reorder/reshape confirmation | test files | testing |
| **E′** | **GcState `active_buffer` retention across runs** (the live heap migrates off `Nursery` into a per-run-dropped thread-local on first GC — must be machine-owned) | `host_fns.rs`, `jit_machine.rs` | codegen |
| **N** | **Session DataConTable accumulation** (merge per-turn tables; pass merged table to render + run, else old custom-ADT values render `<unknown>`) | session mgr, `tidepool-repr/src/datacon_table.rs`, `render.rs` | mcp/repr |

## Review findings (2026-06-25) — applied to the specs below

Adversarial code-grounded review (verdicts: sound / risky / broken). Corrections folded into
the relevant waves; the load-bearing ones:

- **[SOUND] Item 1 — JITModule multi-round define/finalize WORKS** (cranelift 0.129.1, verified in
  installed source: `finalize_definitions` drains via `mem::take` and is idempotent; a *new*
  FuncId post-finalize carves a fresh arena segment leaving round-1 code stable; `pipeline.rs`
  already `mem::take`s `pending_stack_maps`). **The #1 feasibility gate is OPEN.** Caveat: no
  FuncId *re*-definition in 0.129.1 — we add new fragments with new ids (our path), so fine.
- **[BROKEN] Item 4 — the live heap migrates OFF the machine into a per-run-dropped thread-local.**
  After the first GC, `perform_gc` Cheney-copies into a fresh `Vec<u8>` stored in
  `GcState::active_buffer` (`host_fns.rs:623`) and repoints `alloc_ptr` there (`:628`); the
  machine's `Nursery` field is then **stale**. `RegistryGuard::drop`→`clear_gc_state`
  (`host_fns.rs:186-191`) **frees `active_buffer` and clears roots every run** → persisted
  pointers dangle. So "keep the machine alive ⇒ heap stays alive" is **false post-GC**. → new
  component **E′**; 1.A must take ownership of `active_buffer` and split `clear_gc_state` into
  "clear pointers" (per-run) vs "free buffer" (machine-drop). The Converge proof test **must force
  a GC that actually swaps `active_buffer`**, or it silently passes pre-GC.
- **[BROKEN] Item 5 — DataConTable accumulation is a missing component.** Render keys on
  `table.name_of(id)` (`render.rs:77`); a missing id makes even lists/Bool/Maybe/Text fall through
  to the generic branch. Core constructors recur (stable hashes) so `[Int]`/`Bool` survive, but a
  **user ADT** value rendered/`case`d in a later turn needs its `DataConId` in *that* turn's table.
  No `merge`/`extend` on `DataConTable` exists (only `insert`/`insert_checked`). → new component
  **N**; Wave 3 passes the *merged* session table to `value_to_json`/`run`.
- **[BROKEN → RESOLVED BY DESIGN] Item 6 — sessions are a SEPARATE entry point, not in the eval
  pool (decision 2026-06-25).** The original sin was retrofitting a stateful session into the
  stateless concurrent-eval machinery: the eval permit is held for the thread's whole life
  (`server.rs:541`), and `evict_oldest_continuation` (`server.rs:103-120`) only aborts `Paused`
  kinds, so a resident session worker would deadlock 1 of `MAX_CONCURRENT_EVALS=4` and an eviction
  would orphan its thread + leak the permit + strand a 64 MB+ machine ("Server busy" wedge).
  **Fix: don't share the pool.** A session is a separate entry point — one resident thread driven
  forward command-by-command (GHCi-shaped) — that **never touches `eval_semaphore` or
  `evict_oldest_continuation`.** The stateless `eval` tool keeps its semaphore + spawn-per-eval
  unchanged. Sessions *reuse the parked-thread-on-a-channel mechanism* (`EvalSession` is already a
  continuation record) but in a **separate session registry** with its own bounded lifecycle. See
  the rewritten Wave 2.
- **[RISKY → SEQUENCED] Item 2 — 1.A and 1.B BOTH rewrite the `jit_machine.rs` lifecycle seam**
  (`install_registries` `:177`, `RegistryGuard::drop`, the `run` prologue) — `run_fragment` reuses
  the very registry path 1.A rewrites; there is no clean seam. **Decision (2026-06-25): don't
  parallelize the codegen spine — run 1.A → 1.B SEQUENTIALLY** (separate commits/PRs; 1.A lands the
  lifecycle + heap-ownership foundation, 1.B builds re-entry on settled ground). 0.1 still freezes
  the `install_registries`/`clear_gc_state`-split signatures so 1.B codes against a stable shape,
  but the *bodies* land in order, not in two racing worktrees.
- **[RISKY → SCAFFOLD-FIRST] Item 3 — `compile_expr` signature change hits ~30 (mostly test) call
  sites** — do the signature change + empty-env threading as a **dedicated scaffolding commit in
  Wave 0** (mechanical, touches many files but trivially), so 1.B's worktree only adds resolution
  logic, not a workspace-wide signature churn. Correctness invariant to state loudly: **a seeded
  heap pointer is re-materialized as a fresh `iconst` at the Var-miss site each fragment (Cranelift
  SSA values are per-function) — never pre-populate `EmitContext.env` with a shared
  `SsaVal::Value`.**

**General principle from the 2× risky (user, 2026-06-25):** where a parallel split is conflict-
prone, prefer **more granular scaffolding commits before fan-out** (freeze contracts + do the
mechanical signature churn up front) or **sequential sub-waves** — not optimistic parallel lanes
on a shared file. Fan out only where files are genuinely disjoint (that's Wave 0 + Lane A).
- Cache-key session-id (R5) promoted from "buried in B" to a hard item. No locked-decision conflict
  found; reorder-safety claim re-confirmed against `case.rs`/`Translate.hs:1466`.

## Dependency DAG

```
                    ┌─────────────── 0.1 CONTRACTS (Session struct, method stubs,
                    │                 PipelineResult+type field, BindingTable iface) ── must compile
                    ▼
   ┌──────────┬──────────────┬───────────────┬──────────────┐   (Wave 0 — parallel, file-disjoint)
   ▼          ▼              ▼               ▼              ▼
 0.5 compile_expr   A decl-accum   H type-capture   M tests   (C/D/E + lifecycle-seam
 sig scaffold       (SHIPPABLE)    (haskell)        (additive)  stubs already from 0.1)
   │                    │              │
   └────────┐           │              │     ┌──── Wave 1 (codegen spine — SEQUENTIAL) ────┐
            ▼           │              │     ▼                                              ▼
        1.A buffer-retain+roots+GC+cursor (E′+D+E+F)  ───────►  1.B re-entry+env-seed (C+K)
                        │              │                              │
                        │              │                              ▼  CONVERGE: proof test
                        │              │                       (frag-1 tenure → real-GC swap → frag-2)
                        │              │                              ▼
                        │              │      Wave 2: G  session = SEPARATE entry point,
                        │              │               resident driven thread (needs 1.A+1.B)
                        │              │                              ▼
                        └──────────────┴──────► Wave 3: J+I+K-wire + N(table-merge)  (needs G + H + C)
                                                               ▼
                                     Wave 4: B2 reshape · x#h · :t/:i · GC-reachability cleanup · gates
```

Hard edges: 0.1 → everything; 0.5 → 1.B. 1.A → 1.B → converge → 2 → 3. H → 3. A is independent
(ships alone). The codegen spine (1.A→1.B) is serial by the shared lifecycle seam.

## Honest parallelism assessment (revised 2026-06-25)

Deep-not-wide, and after the review the honest peak is **Wave 0 only**. Wave 0 fans out to ~4–5
genuinely file-disjoint lanes (contracts, the mechanical `compile_expr`-signature scaffolding,
Lane A, type-capture, tests). **Everything after Wave 0 is serial in *scheduling*** — the codegen
spine (1.A→1.B) shares the `jit_machine.rs` lifecycle seam (review item 2), and Waves 2–3 are one
cohesive session manager — **but every unit still runs through scaffold→spawn→integrate**: each is
a spawned subagent reviewed at its merge boundary, just spawned *sequentially* rather than in
parallel. So past Wave 0 the swarm benefit is realized as **per-merge adversarial review on every
unit**, not as wall-clock width. The real wins: (1) **Lane A ships independently and early** — a
useful interactive-decl REPL with zero codegen risk; (2) Wave 0 de-risks + does the mechanical
churn up front so the serial spine that follows is conflict-free spawn targets; (3) every unit —
parallel or serial — gets the merge-boundary review. Right goal here: reviewable
scaffold→spawn→integrate sequencing, not optimistic width.

---

## Wave 0 — contracts + independent lanes

### 0.1 — Contracts & scaffolding (root inline, or 1 dev; BLOCKS the wave)
- **ANTI-PATTERNS:** Do NOT implement bodies — stubs only (`todo!()`/identity). Do NOT change
  behavior of the existing one-shot eval path. Do NOT add the session worker yet.
- **READ FIRST:** `plans/ghci-session-persistence.md` (round-3 section + Refined architecture),
  `tidepool-mcp/src/server.rs:74,454-642`, `tidepool-mcp/src/ask.rs:42-189`,
  `tidepool-codegen/src/jit_machine.rs:65-75,154-186`, `tidepool-runtime/src/lib.rs:96-176`.
- **STEPS:** (1) Define `SessionId`, `Session { machine, bindings, decl_log, gen: u64, type_env }`
  as a struct in mcp (fields can be placeholder types). (2) Add stub methods on
  `JitEffectMachine`: `add_function(&mut self, name, &CoreExpr, external_env: &ScopedEnv) ->
  Result<FuncId>`, `run_fragment(&mut self, FuncId, …) -> Result<Value>`,
  `register_persistent_root(&self, slot)` — all `todo!()`. (3) Extend `compile_expr` signature
  with `external_env: &ScopedEnv` (unused for now; existing call sites pass an empty env). (4)
  Extend `PipelineResult` (haskell side contract) + the Rust `compile_haskell` return with an
  optional `captured_type: Option<String>` field (None for now). (5) Define `BindingTable` trait
  shape: `name → (valueId, root_slot, type_string)`. (6) **[review item 2/4] Freeze the
  `jit_machine.rs` lifecycle seam as the contract both Wave-1 lanes share:** document
  `install_registries`'s shape, a `PERSISTENT_ROOTS` + `active_buffer` retention point, and split
  `clear_gc_state` into a `clear_run_scratch()` (per-run) vs `free_session_heap()` (machine-drop)
  signature pair (stubs). This is what lets 1.A (owns the bodies) and 1.B (`run_fragment` calls the
  same path) not collide.
- **VERIFY:** `cargo check --workspace`; existing tests still pass (no behavior change).
- **DONE:** Workspace compiles; all new surfaces are stubs; one-shot eval unchanged; the lifecycle
  seam + `clear_gc_state` split are frozen signatures.

### 0.2 — Lane A: declaration accumulation (SHIPPABLE; 1 TL or dev)
- **ANTI-PATTERNS:** Do NOT touch codegen or the machine. Do NOT persist runtime *values* (text
  decls only). Do NOT mutate a global lib in-place mid-compile (regenerate whole, atomic write).
- **READ FIRST:** `tidepool-mcp/src/eval_prep.rs:139-208` (template_haskell, `helpers` hook),
  `tidepool-runtime/src/lib.rs:130-131` (include dirs → `--include`), `cache.rs:16-37`,
  `.tidepool/lib/Library.hs` (whole-module re-export facade shape), `tidepool-mcp/CLAUDE.md`
  (include precedence `[effects, stdlib, project-lib, global-lib]`).
- **STEPS:** (1) A per-session `.tidepool/session-<id>/Session.hs` module on the include path at
  highest precedence. (2) Maintain an ordered **decl log** (functions, type sigs, `data`/`type`);
  regenerate the whole module from it each turn (invariant: module = pure fn of decl log). (3)
  Detect decl-vs-expression at the eval surface (a turn that is a top-level decl appends to the
  log + recompiles the module; an expression evals as today). (4) Cache key += `session id` (the
  include-dir fingerprint already covers content; the id disambiguates sessions). (5)
  Function/type redefinition = latest text wins (regenerate).
- **VERIFY:** integration test: turn 1 defines `slug t = …`; turn 2 calls `slug "a b"` → works.
  Turn 3 redefines `slug`; turn 4 sees the new one.
- **DONE:** Functions/types/sigs accumulate and are callable across turns; redefinition shadows;
  zero codegen changes. **This is a usable deliverable on its own.**

### 0.3 — H: GHC type capture (1 dev, haskell)
- **ANTI-PATTERNS:** Do NOT make the GHC session persistent (stay batch). Do NOT block on
  rendering perfection — a string is fine for v1.
- **READ FIRST:** `haskell/src/Tidepool/GhcPipeline.hs:38-141` (esp. `:119` typecheckModule),
  `haskell/CLAUDE.md` (extract-session constraints), the `PipelineResult` definition.
- **STEPS:** (1) After `typecheckModule`, locate the `__user` binding's `Id`; read `idType`. (2)
  Render via `renderWithContext defaultSDocContext (ppr ty)` (pattern already used at
  `GhcPipeline.hs:257` for `dumpCore`). (3) Thread it into `PipelineResult` → CBOR → the Rust
  `captured_type` field (contract from 0.1). (4) Note the `ppr`-not-parser-faithful risk in a
  comment (Wave-4 may need a structured `IfaceType`).
- **VERIFY:** extract a known expr (`[1,2,3] :: [Int]`), assert `captured_type == "[Int]"` (or
  GHC's rendering thereof).
- **DONE:** Every eval returns the inferred type of its top expression as a string.

### 0.4 — M: regression test harness (1 dev/haiku; additive)
- **ANTI-PATTERNS:** Do NOT modify non-test code. Tests may be `#[ignore]` until features land.
- **READ FIRST:** `tidepool-codegen/src/emit/case.rs:380-457` (runtime_case_trap path),
  `tidepool-codegen/src/host_fns.rs:2943+` (graceful CaseTrap), existing repro tests in
  `tidepool-runtime/tests`.
- **STEPS:** (1) Regression test asserting a `DataConId`-mismatch case yields a clean
  `CallToolResult::error` (CaseTrap), NOT a process crash. (2) Scaffold (ignored) a
  reorder/reshape-across-turns confirmation test for when value-binding lands.
- **VERIFY:** the graceful-trap test passes now; the reshape test compiles (ignored).
- **DONE:** Graceful-trap guard green; reshape scaffold in place.

### 0.5 — `compile_expr` signature scaffolding (1 dev; mechanical, do BEFORE Wave 1)
- **ANTI-PATTERNS:** Do NOT add resolution logic (that's 1.B) — only the threaded param + empty-env
  pass-through. Do NOT change behavior.
- **READ FIRST:** `emit/expr.rs:1607-1669` (compile_expr), `emit/mod.rs:128-195` (ScopedEnv), and
  every `compile_expr` call site (~30, mostly codegen tests + `tidepool-testing` + `jit_machine.rs:154`).
- **STEPS:** Add `external_env: &ScopedEnv` to `compile_expr`; thread it into `EmitContext`; pass an
  empty env at every existing call site. This is the workspace-wide mechanical churn — landing it
  as its own commit means 1.B's worktree only adds the Var-miss resolution logic, never the
  signature change (kills the review-item-3 churn-conflict).
- **VERIFY:** `cargo check --workspace`; all tests pass (no behavior change).
- **DONE:** `compile_expr` takes `external_env`, ignored everywhere; clean base for 1.B.

---

## Wave 1 — codegen spine (SEQUENTIAL: 1.A → 1.B → converge)

Not parallel lanes — 1.A and 1.B both rewrite the `jit_machine.rs` lifecycle seam (review item 2),
so they land in order as separate commits. 1.A is the foundation (heap ownership + roots + GC);
1.B adds re-entry on top of the settled lifecycle.

### 1.A — Buffer retention + persistent roots + tenuring + cursor (E′+D+E+F) (1 strong TL/dev)
- **ANTI-PATTERNS:** Do NOT add a write barrier (immutable strict-forced values don't need one —
  document the invariant). Do NOT break the existing one-shot GC path (fast-path tests must stay
  green). Do NOT touch `emit/expr.rs` or `add_function` bodies (that's 1.B) — but you OWN the
  `install_registries`/`RegistryGuard`/`clear_gc_state` bodies behind the 0.1-frozen signatures.
- **READ FIRST:** `host_fns.rs:70-102,104-130,186-191,535-657` (esp. `:623` `active_buffer`
  assignment, `:628` cursor repoint), `tidepool-heap/src/gc/raw.rs:124-162` (`:136,154` is_in_range,
  `:139` root update), `nursery.rs` (make_vmctx), `jit_machine.rs:117-130,177-183` (RegistryGuard,
  install_registries).
- **STEPS:** **(E′ — DO FIRST; the load-bearing fix)** the live heap is in `GcState::active_buffer`
  after the first GC, not the machine's `Nursery`. Make `active_buffer` **machine-owned** (move it
  onto `JitEffectMachine`, or hand survivors to a session-owned `old_space` *before* teardown);
  implement the 0.1 split so `clear_run_scratch()` runs per-`RegistryGuard::drop` but
  `free_session_heap()` runs only at machine drop — so a GC between two fragments no longer frees
  the heap or wipes roots. (D) `PERSISTENT_ROOTS` thread-local parallel to `RUST_ROOTS`;
  `perform_gc` appends it to root_slots; `register_persistent_root` fills the 0.1 stub. (E) Split
  `GcState` into nursery (gen-0) + append-only growable `old_space` (gen-1); `tenure(ptr)`
  evacuates a strict-forced closure once into old_space and registers its root; minor GC from-range
  = nursery only (old_space auto-skipped by `is_in_range`); major old_space compaction only on
  explicit call. (F) Thread a session high-water mark through `make_vmctx` (nursery survivors are
  pre-tenure only).
- **VERIFY:** `cargo test -p tidepool-codegen`; new unit tests: **(critical) a pointer survives a
  forced GC that swaps `active_buffer`, then a second run reads it correctly**; tenured root
  survives N minor GCs untouched; minor GC cost independent of tenured count; persistent root
  survives a `RegistryGuard` drop; one-shot fast-path tests still green.
- **DONE:** `active_buffer` is machine-owned across runs; roots split run/session-scoped; bindings
  tenure into old_space; minor GC is O(nursery).

### 1.B — Re-entry APIs + env-seeding + strict-force (C+K) (1 dev)
- **ANTI-PATTERNS:** Do NOT modify `RegistryGuard`/GcState/`host_fns.rs` GC (that's 1.A). Do NOT
  reset the module after first finalize without re-finalizing. Keep the one-shot `compile`+`run`
  path working.
- **READ FIRST:** `jit_machine.rs:65-75,154-166,186-220`, `pipeline.rs:128-182`,
  `emit/expr.rs:368-410,1607-1669`, `emit/mod.rs:128-170` (ScopedEnv), `types.rs:4`
  (ERROR_SENTINEL_TAG 0x45; externals 0xFE).
- **STEPS:** (C1) `add_function`: declare+define a new fragment into the existing JITModule,
  re-`finalize_definitions` (verified multi-round-safe in cranelift 0.129.1; new FuncId carves a
  fresh arena segment, round-1 code stays put); return its `FuncId`. (C2) `run_fragment(func_id, …)`
  like `run` but targets a given entry, reusing the live machine-owned heap (via 0.1's
  `install_registries`/buffer-retention contract — do NOT re-implement teardown). (C3) thread
  `external_env` through `compile_expr`→`EmitContext` (~30 call sites, mostly tests — pass empty
  env). (C4) `SsaVal::from_external_pointer(*const u8)` reusing the poison-ptr `iconst` template.
  (K) `deep_force` the run result to NF before binding (existing iterative deep paths; no recursion).
- **CORRECTNESS INVARIANT [review item 3]:** a seeded heap pointer is re-materialized as a **fresh
  `iconst` at the Var-miss site** (`emit/expr.rs:368-410`, parallel to the `0x45` sentinel branch)
  *each fragment* — Cranelift SSA values are per-function. **Never** pre-populate `EmitContext.env`
  with a shared `SsaVal::Value`; the env carries the pointer-as-`*const u8`, lowered per use.
- **VERIFY:** unit test: compile fragment-1, get a heap ptr; seed env `{VarId→ptr}`; `add_function`
  fragment-2 referencing that VarId; `run_fragment`; assert correct value.
- **DONE:** A second fragment JITs into a live machine and resolves a seeded heap pointer.

### Converge (root or 1 dev) — mechanical smoke check ONLY (not the acceptance proof)
- After merging 1.A + 1.B: a low-level Rust test that `add_function`/`run_fragment`/seed/tenure work
  at the API level — build fragment-1, tenure + `register_persistent_root`, seed env, `add_function`
  fragment-2 `case x of …`, `run_fragment`, assert correct; a variant that allocates past the
  nursery high-water mark so a **real GC swaps `active_buffer`**, asserting the root relocated.
- **This is a SMOKE check to catch gross breakage early — explicitly NOT the proof of the feature.**
  Per the testing principle below, the real acceptance proof drives the actual session entry point
  over multiple turns with natural GC (Wave 2 precursor + Wave 3 headline). Do not let the smoke
  test stand in for that — a bespoke harness path can pass while the real path is broken.

---

## Wave 2 — `tidepool-repl`: a SEPARATE MCP server (G) (1 TL/dev; serial)

Decision (2026-06-25): a session is NOT a kind of eval-in-the-pool, and NOT new tools on the
existing `tidepool` server — it's a **fresh, separate MCP server/binary, `tidepool-repl`** that
reuses `tidepool-runtime`/`tidepool-codegen` (the machine, binding table, GC) but exposes its own
interface. One resident thread, driven forward command-by-command (GHCi-shaped). The existing
`tidepool` eval server is **untouched** — no risk of destabilizing it. MVP = **one active session**.

- **ANTI-PATTERNS:** Do NOT add session tools to the existing `tidepool-mcp` server. Do NOT route
  through `eval_semaphore`/`continuations` (different server entirely). Do NOT spawn-per-command
  (one resident thread). Do NOT touch the session machine from a non-owner thread (GC/root/stream
  state is thread-local — affinity is correctness). Do NOT allow >1 active session in the MVP.
- **READ FIRST:** `tidepool-mcp/src/ask.rs:42-53,168-189,223-262` (the parked-thread-on-a-channel
  mechanism to REUSE), `tidepool-mcp/src/server.rs` (the server skeleton to mirror, NOT extend),
  `tidepool-runtime` (the machine/compile APIs to reuse), round-3 R1/D1/D4.
- **STEPS:** (1) New crate `tidepool-repl` (MCP server binary) reusing `tidepool-runtime`/`-codegen`.
  (2) **Explicit tool surface (decision #3):** `session_open` · `session_def` (append a
  declaration — function/type/sig — to the session library) · `session_eval` (evaluate an
  expression; `x <- e`/`let x = e` binds) · `session_cmd` (meta: `:t` `:i` `:bindings` `:reset`) ·
  `session_close`. (Later: collapse to one `session(input)` that classifies by leading `:` →
  meta-cmd, else parse decl-vs-expr — the GHCi auto-feel; see suggestion in chat.) (3) A **session
  manager** holds the one session: a resident worker thread owning the live machine (Wave 1) +
  binding table + decl log + merged DataConTable; commands arrive on a single-consumer channel
  (**serialized by construction**). (4) Reuse the parked-thread mechanism: an in-command `ask`
  suspends the session thread; the answer arrives as the next command (the session IS the
  continuation, in its own registry). (5) Per-command timeout via the existing `PauseGate`; the
  *session* persists across commands. (6) `session_close`/idle-timeout aborts the worker and **drops
  the machine** (`free_session_heap`) — self-contained lifecycle.
- **VERIFY (acceptance proof — correctness sweep through the REAL path, natural GC; decision #1):**
  a test that `session_open`s and drives **many real turns** exercising **all three persistence
  tiers**: (a) bind an **Int**, read it back several turns later (Tier-0 forced scalar); (b) bind a
  **JSON `Value`**, slice/transform it later (Tier-0 forced structure + DataConTable-merge for
  rendering); (c) bind a **function** `f`, **call `f` in a later turn** (Tier-1 live closure — this
  specifically proves fragment-1's code pointer stays callable after `add_function` adds later
  fragments, per the cranelift arena property). Interleave enough allocation that **GC fires
  organically** (do not force it); assert every binding still reads/calls correctly *after* a
  natural collection (real exercise of E′/`active_buffer`). Plus: an `ask` in a command
  suspends/resumes via the next command; `session_close` frees thread+heap with no orphan. The
  Wave-1 smoke check is necessary but NOT sufficient — this sweep is. **The goal is correctness
  coverage across value categories, not a demo scenario.**
- **DONE:** `tidepool-repl` is a standalone MCP server: one resident driven thread, a live machine
  across many turns surviving natural GC, Int/JSON/function bindings all correct, self-contained
  lifecycle, zero coupling to the `tidepool` eval server.

## Wave 3 — value binding end-to-end (J+I+K-wire) (1 TL, maybe 1 pair; needs G+H+C)
- **ANTI-PATTERNS:** Do NOT let GHC try to supply the value (it never needs it — decouple type
  plane from value plane). Do NOT reuse a valueId across binds. Use `OPAQUE`, not `NOINLINE`.
- **READ FIRST:** round-3 H1/H3/C1/D2, `emit/expr.rs:368-381` (high-byte tag dispatch),
  `eval_prep.rs:139-208`, `datacon_table.rs:73-85` (collision guard).
- **STEPS:** (J1) binding table `name → valueId(0xFD-tagged, counter-minted) → root`; rebind
  repoints the name map, old root stays. (J2) `0xFD` resolution in `emit/expr.rs` Var dispatch →
  seeded pointer from the binding table. (I) on `x <- action` / `let x = e`: **(K — tier-aware
  bind)** evaluate the RHS; if the result is **first-order data**, `deep_force` to NF then tenure;
  if it's a **closure/function/PAP (Tier-1)**, do NOT deep-force (forcing a function is identity) —
  store the closure as-is, register its root + its captured-env roots, and rely on machine-code
  liveness (valid because the session keeps the machine alive and `add_function` leaves prior code
  segments stable). Then mint valueId, record captured type (H). On later reference, inject
  `x :: <type>; {-# OPAQUE x #-} ; x = <placeholder>` into the session module so GHC typechecks,
  and seed the env so the JIT overrides x's VarId with the real root. (audit: `0xFD` low-56-bits
  must not alias a real fingerprint external — start the counter high; check vs
  `TIDEPOOL_VARID_AUDIT`.) **Tier-1 is in MVP scope** because the acceptance sweep binds a function.
- **(N) [review item 5] Merge the DataConTable across turns:** the session holds an accumulated
  table (union of per-turn tables via `insert_checked`); pass the **merged** table to
  `machine.run`/`run_fragment` and `value_to_json` — else a turn-1 custom-ADT value rendered in
  turn 3 keys on a missing id and falls through to the generic `<unknown>` branch (`render.rs:77`).
  Core constructors recur with stable hashes so `[Int]`/`Bool`/`Maybe`/`Text` already survive;
  user ADTs are the ones that need the merge.
- **VERIFY (headline acceptance proof — through the REAL entry point, multi-turn, natural GC):** a
  test driving `session_open` then a sequence of real `session_eval` turns: bind `x <- bigStruct`,
  then over *several later turns* `length x` / `slice x` / `case x of …`, interleaving enough other
  allocation that **GC fires organically** between the bind and the slices — asserting correct
  results and that a custom-ADT `x` from an early turn is **rendered (real constructor names, not
  `<unknown>`) AND case-matched** in a later turn after a natural collection. Same code path as
  production; no hand-wired machine calls, no forced GC.
- **DONE:** `x <- …` then slice/transform/examine `x` across turns, for any strict-forced typed
  value incl. custom ADTs, with correct rendering. **MVP complete.**

## Wave 4 — polish / optional (parallel)
- B2 gen-suffix type-reshape semantics (coexisting `Foo__g1`/`Foo__g3`); `x#h` hash-qualified
  refs; `:t`/`:i` (from H's captured type); GC-reachability-tied root cleanup (replaces leak-for-
  session); promote the reorder/reshape test from ignored → gate; structured `IfaceType` if
  string typechecking proves fragile.

## Critical path & estimate
0.1 + 0.5 → 1.A → 1.B → converge → 2 → 3 = the MVP spine (serial after Wave 0; ~3–4 wk wall).
Wave 0 (contracts, signature scaffold, Lane A, type-capture, tests) is the only parallel stretch.
**Lane A (0.2) ships standalone in days, before the spine** — the early, low-risk deliverable.
