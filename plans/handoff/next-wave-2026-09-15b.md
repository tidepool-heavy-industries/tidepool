# STG cutover: finish step 2, split the work by who does it

Branch `engine/stg-production-cutover`, HEAD `854c9b051`, nothing pushed.
Governing plan: `plans/stg-completion.md` (steps 2–5 remain). Contracts:
`plans/handoff/designs/{resume-contract-v2,lifetime-contract-v2,actor-exit-contract}.md`.
Previous wave doc: `plans/handoff/next-wave-2026-09-15.md` (A1 done there; this
plan supersedes its A2–A7 shape and is to be committed as
`plans/handoff/next-wave-2026-09-15b.md` with the README pointer moved).

## Context

Fable review of the two Opus commits since the last review, then a re-survey of
the runtime owners for A2–A7. The survey found the approved A2/A3 shape was
wrong in two places, and both are structural rather than plumbing. The plan
below (1) records the review, (2) fixes the design where it was wrong, and (3)
splits the remaining cutover into what Fable does directly (the clever and
engine-invariant work) and what later Sonnet waves do (mechanical plumbing,
fixtures, follow-ups), with explicit yield points.

## Part 1 — Review of `534d7c8ea` and `854c9b051`

Verdict: **accept both.**

| Commit | Judgment reviewed | Verdict |
|---|---|---|
| `534d7c8ea` display precedence | `displayTreePrec` + `precedenceParens` (GHC's `showParen` rule); the `Show` fallback uses `showsPrec`; generated cell instances define `displayTreePrec` by record-pattern `case`, naming nothing from the cell's scope. | Correct, matches derived `Show`, including infix and record constructors. One deliberate deviation: as an argument, `Text` containing whitespace is parenthesized rather than quoted (`WatchReady (Right (Remove the …))` unchanged). Product choice, recorded in the commit. |
| `854c9b051` A1 | Tag 39 with no MAGIC bump; one `PreparedStg` compile yielding both halves; `read_prepared_program` → `MissingOutput` when absent. | Correct. Verified from source: a projection rejection is `SourceRejection` (`DiagJson.hs:39`), not a GHC `SourceError`, **and** is raised in `writePreparedArtifacts` after `compileVariants` has returned (`Main.hs:478-482, 596, 608`), so it can never drive the template fallback and Core is never a silent substitute. `ConstructorDecl.host_id` is the same bridge `DataConId` the Core table mints (`execution_schema.rs:289-291`), so A1's "both halves" gives the notebook one `DataConTable` for both engines. |

## Part 2 — What the survey changed (source facts, `854c9b051`)

1. **The actor host never dispatches a turn through `ActorRunTarget`.** The
   trait (`tidepool-actor/src/mount.rs:221`) has two methods,
   `install_actor_execution` and `retire_placement`. Every cell runs through
   concrete `ResidentSession` methods: `run_with_sites`, `run_bind_with_sites`,
   `run_observation_with_sites`, `run_projected_bind_with_sites`
   (`resident_workbench.rs:2444-2472`), plus `stage_declarations_in`,
   `lease_bindings`, `publish_captured_alias_in`, `next_value_generation`,
   `reserve_value_generations_through`. `SessionRegistry<M, H>` puts **no
   bound on `M`** (`registry.rs:167,182`); the monomorphization is
   `ActorMachineRegistry = SessionRegistry<ResidentSession<H,O>, String>`
   (`resident_workbench.rs:194`). An engine enum at the registry would need a
   ~10-method session trait and a second implementation of the notebook
   bookkeeping (scopes, generations, provenance, leases) — a duplicated
   mechanism.
2. **The notebook bookkeeping already has a prepared-shaped slot.**
   `PersistentSession` (`persistent.rs:80-121`) owns `machine:
   Option<JitEffectMachine>`, `session_table: DataConTable`, `bindings:
   BindingTable`, `val_gen`, `scopes`, `effect_policy`, `live_payload`.
   `BoundValue::Prepared { root, handle, origin }` already exists in that same
   `BindingTable` (`binding_table.rs:90-111`). `PreparedRuntime`
   (`prepared.rs:242-275`) re-implements a subset of this (own `bindings`,
   `val_gen`, `binding_ids`, `realm_leases`, `ScopeId::ROOT` placeholder,
   stored-and-ignored policies) — it is the duplicate, not the target.
3. **A multi-binder bind projects as one tuple entry `__result`**
   (`Main.hs:605-610`, `workbench.rs:626` tail `({{BINDERS}})`); the Core side
   splits it in `mkBoundBinders` (`SessionArtifacts.hs:33-42`) and there is no
   prepared-side split. Prepared components must be bound from the settled
   result's fields, with a host-minted `SymbolIdentity` matching what a later
   turn's `GlobalDecl::identity` names (`Val.G<g>`, `x`).
4. **`Tidepool.Inspection` is not in the prepared corpus** (`haskell/test/Suite.hs`
   imports: Prelude, Text, QQ, Patch, Render, Double, Aeson.Value). The cell
   render (`render_cell_observation`, `resident_workbench.rs:2780`) is three
   compiled Core turns per displayed cell; under prepared STG it is unproven.
5. **Suspension machinery on the prepared engine**: `ResourceLedger` is
   embedded and fully capable (`park`/`take_continuation`/`close_realm`), only
   the `debug_assert` at `machine.rs:842` keeps it unused. `FrameEvidence`,
   `TypeNode`, `HostUnconstructible`, `Tidepool.Internal.Resume`, and an
   interner `by_host` index do **not** exist. `marshal_descriptor_object`
   (`descriptor_bridge.rs:43`) is a complete, tested, caller-less builder. The
   test resume path resolves `E`/`Val`/`Union` by bare occurrence
   (`prepared_execution.rs:517-530`) — the ambiguity `freer_names::resolve`
   exists to avoid.
6. **Residency**: nothing carries a `ProgramId`; `ProgramId` is a vector index;
   no retire path; `TopSlotBase`/`claimed_slots`/`vmctx.prepared_tops`
   exactly as the lifetime contract describes; `stage_compaction`/
   `commit_compaction` live in codegen `old_space.rs` (Core-only caller
   `collect_major_quiescent`), not in `tidepool-heap`; no `Quiescent` type.
7. Pre-existing, unrelated to the wave: `proptest_cache_layer` 17 failures
   because the property harness's fake extractor writes no prepared artifact
   (`stg-completion.md:217-222`); `value_to_heap` truncates a `Con` field count
   to 16 bits (`proptest_boundary_roundtrip.rs:1027`).

## Part 3 — Design decision: the engine seam lives inside `PersistentSession`

**Decision.** The migration route is a field, not a type parameter:

```rust
// tidepool-runtime/src/session/persistent.rs
enum SessionEngine { Core(JitEffectMachine), Prepared(PreparedMachine<'static>) }
pub enum EngineKind { Core, Prepared }           // chosen at construction, never switched
struct PersistentSession { engine: Option<SessionEngine>, engine_kind: EngineKind, … }
```

- `PersistentSession::new(lib, nursery_size)` gains `engine_kind`;
  `ResidentSession::unbootstrapped`/`new` thread it. Production construction
  sites: `tidepool-harness/src/harness.rs:932,4141`,
  `selfharness/driver/lifecycle.rs:219`,
  `tidepool-actor/src/resident_workbench.rs:6477`. The temporary explicit
  route is one env/config read at those composition roots
  (`TIDEPOOL_ENGINE=prepared`), resolved once per session and recorded in the
  session's receipt/provenance so a transcript says which engine ran it.
- The run methods on `ResidentSession` take the **compiled turn** rather than
  `(expr, table)`: `run_*_with_sites(&CompiledTurn, …)`. The Core arm reads
  `expr`/`table` as today; the Prepared arm requires `compiled.prepared` and
  errors (`ResidentError::Run`, no fallback) if it is `None`. `merge_table`
  runs for both arms (one `DataConTable`, fact 1 of Part 1). `on_eval_thread`'s
  closure type becomes `FnOnce(&mut SessionEngine, …)`.
- `TurnRequest.prepared` is `Some` iff `engine_kind == Prepared`, with the
  retained set derived from `bindings.iter_live()`'s `BoundValue::Prepared`
  entries (fold of `prepared_turn.rs:129-144` into `PersistentSession`). The
  production `TurnRequest` sites (`resident_workbench.rs:6045-6055`, `:2737`)
  ask the session for `prepared_turn()` instead of hardcoding `None`.
- Prepared bind/observation: run the entry through `PreparedMachine::
  run_entry_retained`, settle (Wave A5 makes this the shared routine; before
  A5 a run whose result is `E` is a typed `ResidentError::Run` "effects are
  not yet supported on the prepared route"), then bind: single binder →
  `BoundValue::Prepared` on the result handle; N binders → `inspect_outer` the
  tuple, retain each field as a handle, bind each with identity `{ unit: <the
  program's entry unit>, module: binder.module_name, namespace: "value",
  occurrence: binder.name, record_parent: None }` and the binder's generation.
  `PreparedOrigin` becomes `PreparedProvenance { identity, generation, origin:
  Option<(ProgramId, ValueId)> }` so an import resolves against a bound
  component exactly as against a top. Scope is the real `run_context.
  lexical_scope`; `ScopeId::ROOT` placeholder gone.
- Consequences: `PreparedRuntime`'s own `bindings`/`val_gen`/`binding_ids`/
  `realm_leases`/`actor_execution` are deleted; what remains of
  `session/prepared.rs` is the link/compile/install/retain helpers over
  `PreparedMachine` that `PersistentSession` calls (rename to what it is).
  `impl ActorRunTarget for PreparedRuntime` (`mount.rs:277-311`) is deleted;
  `ResidentSession` is the one mounted type on both routes. `SessionTurns`,
  `TurnForm`, `project()`, `prepared_turn_module` are deleted.
  `set_actor_execution`'s stored-and-ignored policies disappear because the
  prepared arm reads `PersistentSession.effect_policy`/`live_payload` — the
  same fields Core reads.

**Why not the registry-level enum the previous plan named:** it would
re-implement the value plane, scope tree, generations, and leases a second
time (repo rule: one mechanism), and would still leave the cell path calling
concrete methods. The seam here is where the two engines actually differ:
"add code and run it", nothing else.

## Part 4 — Work split and order

Legend: **F** = Fable does it directly; **S** = a Sonnet wave (Haiku for pure
mechanical sweeps); **↑** = Fable review point / upshift; **↓** = downshift.
Every item: failing test first, focused runs only, `cargo fmt`, crate-scoped
clippy `-D warnings`. No broad battery until the Wave A exit.

### F0. A2 probe (first action after approval; nothing else starts before its result)

**Status: done**, folded into `b8d25637f` (`tidepool-actor/tests/prepared_render_probe.rs`).
All three cases (render bind, dialect expression, opaque fallback) project;
outcome (a) did not fail, so F1 proceeded as planned.

One temporary extractor-gated test beside `tidepool-runtime/tests/run_llm_turn_sidecar.rs`
(it already assembles a real preamble via `tidepool_mcp::build_preamble`/
`ensure_effects_module`), running `run_turn` with `prepared: Some(PreparedTurn
{ retained: &[] })` over the real `resident_workbench_templates`, for:
(a) the `DisplayPage`-typed **bind** `__tidepoolPage1 <- pure
((TidepoolInspection.displayPageWithout [] 8192 (obs ())) ::
TidepoolInspection.DisplayPage <row>)` where `obs` is a small record/`Either`
value; (b) a dialect-sensitive **expression** exercising defaulting and
`OverloadedStrings` (`length (show (2 ^ 10)) + T.length "abc"`); (c) the
opaque fallback rendering (`pageWithContinuation … TextLeaf …`). Assert each
yields `compiled.prepared.is_some()` and `compiled.prepared.constructors()`
`host_id`s are a subset of `compiled.table`'s ids (fact 1, Part 1). Outcomes:
all three project → proceed as planned; (a) fails → bring it to the user
before F1 (the render path's shape changes: either the render module is
restricted to what projects, or the display turn keeps a Core-side renderer
for the transition — user decision, not a workaround). The probe file is
deleted at F3, its assertions folded into `tests/prepared_turn.rs`.

### F1. Engine seam (Part 3), Expr + single Bind + Decl, no effects

**Status: done, committed as `b8d25637f`** ("the engine route is a field of
the resident session"). Evidence in `implementation-2026-09-15.md`'s "Wave A,
step 2" section.

Owners: `session/persistent.rs`, `session/resident.rs`, `session/prepared.rs`
(shrink), `session/mod.rs` re-exports, the four construction sites, the two
production `TurnRequest` sites. Test: `tests/prepared_turn.rs` rewritten on
`resident_workbench_templates` + the real preamble: Decl `f n = n + 1`, Expr
`f 41`, Bind `x <- pure (f 1)`, Expr `x + 1`, with `EngineKind::Prepared`
through `ResidentSession` (not a bespoke driver), asserting the rendered
value and that `BoundValue::Prepared` is what got bound; the same sequence
with `EngineKind::Core` passes unchanged (dual-run, one test body). Registered
in `tests/suites/session.rs`, gated by `require_extract()`.

### F2. Multi-binder bind + cell render on the prepared route

**Status: implemented, landing in the commit after `b8d25637f`; test evidence
pending the gate run.** Evidence in `implementation-2026-09-15.md`'s "Wave A,
step 2" section.

`(x, y) <- …` two-name bind bound from tuple fields with minted identities,
then a turn importing `y`; `render_cell_observation` on `EngineKind::Prepared`
through `resident_workbench` renders `it` in a cell receipt; a failing turn
reports `ResidentError::Run` with the committed prefix intact and no Core
fallback (model: `notebook_prefix_failure.hs`). Owner: `tidepool-actor`
resident_workbench only where it constructs `TurnRequest`s.

### ↓ S1 (Sonnet wave 1, starts after F2 lands; Wave C may start immediately)

Independent of F1/F2:
- **Wave C** (actor exit contract Q1-B/Q2-B, `actor-exit-contract.md`): the
  seven `start_paused` unit tests, `publish_exit` + `ExitAuthority`, one
  shutdown deadline, delete the rewrite at `local_actor.rs:1436-1442`,
  document on `drainActor`. `tidepool-actor` only.
- `proptest_cache_layer`: fake extractor writes a prepared artifact (or the
  17 cases assert the cache's typed refusal) — pre-existing gate breakage.
- Small follow-ups: `render_child_budget(Option<u32>)`; `insert_idle(Box<M>)`;
  `hosted_lifecycle_tests.rs:480` `[available]` assertion; daemon `count(1)`
  true minimum; `num-bigint` → `workspace = true`; `value_to_heap` 16-bit
  field-count truncation → typed error; double-formatting oracle pinned under
  `fixtures-check` + shared `needs_precedence`; `emit.rs:305/325` join-filter
  tests; `resolve_literal_bytes` borrow-across-closure comment.
Dependent on F1/F2:
- Delete `SessionTurns`/`TurnForm`/`prepared_turn_module`/`impl ActorRunTarget
  for PreparedRuntime` and every now-dead re-export; sweep `prepared: None` →
  `session.prepared_turn()` at the remaining test call sites; thread
  `EngineKind` through the harness and self-harness drivers; record the
  engine in the session receipt/provenance rendering.
- `plans/handoff/README.md` file map refresh; `implementation-2026-09-15.md`
  entries for F1/F2.
↑ Fable reviews S1 once (one cycle), then F3.

### F3. Schema 10: site table + structural type evidence (resume decisions 1–3)

Fable designs and writes the producer: `TypePolicy` gains `TypeNode`
normalization (synonyms via `coreView`, families, newtype erasure,
`HostUnconstructible` leaves), `VerbSpec.vsDelivery`, `YieldSite` delivery
mode + root `TypeNodeId`, constructor-closure interning for every reachable
`Data` node, `ExecutionSchema/Encode` site table; Rust codec + validation;
`DescriptorInterner.by_host` refusing divergent `host_id`s;
`SCHEMA_VERSION = 10`. Also: the prepared side resolves `Val`/`E`/`Union` by
**qualified** identity through `tidepool_repr::freer_names` (one name table),
replacing the bare-occurrence scan. **↑ one-way door: Fable review of the
schema shape against the contract before regenerating fixtures.**
↓ S2 after the shape is fixed: codec round-trip tests, schema-9 rejection,
`Either Int Text ≠ Either Text Int`, never-matched `Left` declared, `just
fixtures-update` + the seven prepared fixtures (`FreerRetention.md`), corpus
oracle reseal, `tidepool-mcp` generator emitting `settleEff` per compiled row
from the template Fable writes in F4.

### F4. Suspension and one settlement routine (decisions 4–5)

**Status: slice 1 landed, committed as `97df2d711`** ("park prepared
suspensions in the machine ledger"; design: `designs/prepared-parking.md`).
Remaining F4 items: the generated `settleEff` forcing parcel, synthetic reply
sites for ordinary effects, and live-payload custody on prepared parks. F5
(host-built answers with rollback) is next.

`Tidepool.Internal.Resume.resumeLifted = qApp` (NOINLINE, deployed stdlib);
`settleEff` template; prepared machine gains real parking: on `E`, observe
the `Union` payload through the existing observe path, read the site id,
look up the site row, root `q` + live payload, `ResourceLedger::park` with
`FrameEvidence::Prepared { program, site }` (`ContinuationFrame.table` →
`evidence: FrameEvidence::{Core(Arc<DataConTable>), Prepared{..}}`); the
`machine.rs:842` assertion becomes the real frame count; initial and resumed
completion share one routine. `ResidentSession::resume*` dispatch on the
engine. Test: the `FreerResume` fixture resumed through `resumeLifted`;
`prepared_resident_composite.rs:351` park/resume through the ledger.

### F5. Host-built answers with rollback (decisions 6–8) — touches nursery cursors

`prepared_program/answer.rs` on `marshal_descriptor_object`: size → reserve
once via `prepared_gc_trigger` → external bytes via
`allocate_external_storage` with a rollback list → build bottom-up → publish
as a realm handle; any failure resets the bump cursor and releases payloads.
Peek → validate → build → take → enter in `resume(id, input)`. Slice 1:
nullary/scalar (`Bool`). Fable implements; ↓ S3 writes the six acceptance
rejection tests from the contract (wrong family, swapped `Either`,
out-of-closure constructor, host answer to `LiveReentry`, other-realm/
`Evaluating` handle, injected allocation failure — each leaves the frame
parked, counts unchanged, latch clear; second resume `UnknownContinuation`)
and the A7 end-to-end scenario (`b <- runLLMTurn @Bool "q"; pure (not b)`
parks, resume with `true` completes `false`; `"yes"` and `1` rejected with
the frame resumable; then declare/retain/PAP/park/sibling/resume/lookup).

**Wave A exit:** `tests/prepared_turn.rs` + A7 green; **one** quiet broad
`just verify`; totals recorded in `implementation-2026-09-15.md` and
`stg-completion.md` step-2 evidence; solo-rerun any timeout before
classifying it.

### F6. Wave B — bounded residency, first slice (Fable, after F3; can overlap S2/S3)

Per `lifetime-contract-v2.md` first slice, in this order because each removes
a dependency of the next: decision 4 (`ProgramId` from `MonotonicIdIssuer`,
`programs: BTreeMap`, programless `PreparedMachine::empty`); decision 5
(per-program `RootBlock`, `emit.rs:1550-1594` and `adapter.rs:90-101` become
`iconst(block) + load(local*8)`; delete `TopSlotBase`/`claimed_slots`/
`vmctx.prepared_tops`/`SESSION_TOP_SLOTS`; **keep** a typed refusal of a
second outstanding compile so `an_outstanding_compile_is_refused_…` survives
as the guard); decision 6 (`quiesce() -> Quiescent<'_>`); decision 3 (owner
sets on `prepared_callables`/`prepared_enters`/`descriptor_registry`/
`DescriptorSpace`/interner rows + `prepared_byte_pools`, live-header census);
decision 2 (major mark in `tidepool-heap/src/gc/raw.rs` beside `cheney_copy`;
descriptor-arena compaction reusing `OldSpace::stage_compaction`/
`commit_compaction`); decision 7 (retirement order); decision 8 (receipt with
pins). Test: the 10k-iteration synthetic loop with flat counters after
warm-up. Deferred per contract: edge (c) byte-storage pinning, parked sites,
external sweep, lease re-keying/shadowed-binding policy.
↓ S4: counter reporting plumbing, `tidepool-codegen/CLAUDE.md` root-accounting
text update, full `cargo nextest run -p tidepool-codegen`, `just fixtures-check`.

### Steps 4–5 (unchanged, after A–C): parity under the notebook dialect,
prepared route default for fresh sessions, deletion ledger; cell compile-count
reduction (`designs/cell-compiles.md`) after step 4.

## Yield schedule (when to downshift, when to upshift)

| Point | Model | Why |
|---|---|---|
| F0–F2 | Fable | probe verdict + the seam are the design; get them right once |
| S1 | Sonnet (Haiku for sweeps) | Wave C is fully specified; deletions and plumbing are mechanical |
| ↑ after S1 | Fable | one review cycle, then F3 |
| F3 | Fable | schema 10 is a one-way door |
| S2 | Sonnet | codec tests, fixtures, generator emission from a fixed template |
| F4, F5 | Fable | freer parking and the allocation/rollback protocol are engine-invariant |
| S3 | Sonnet | acceptance tests from the contract's list, A7 assembly |
| F6 | Fable | GC work (memory: `fable-drives-hard-gc-work-directly`) |
| S4 | Sonnet | counters, docs, broad runs |

Sonnet waves get: the exact test names to write, the files they may touch,
and "stop and report" on any engine-invariant surprise. No Opus in
workflows; no Sonnet implementer swarms on F-items.

## Verification

- F0: the probe test passes solo (`just test-target tidepool-runtime
  run_llm_turn_sidecar 'test(<probe>)'`), or its failure text is recorded.
- F1/F2: `tests/prepared_turn.rs` dual-run (Core and Prepared) green;
  `just test-lib tidepool-runtime 'test(session)'`; the three affected
  `tidepool-actor` display tests solo; clippy/fmt clean.
- F3: `cabal test prepared-stg-pipeline-test`, `just fixtures-check` after S2
  regenerates; schema-9 artifact rejected with `UnsupportedVersion`.
- F4/F5: withheld-fix proofs on the parking assertion and the rollback path;
  `prepared_resident_composite.rs`, `prepared_execution.rs` solo.
- Wave A exit: one quiet `just verify`, totals recorded.
- F6: 10k loop counters flat; full codegen suite; `just fixtures-check`.
