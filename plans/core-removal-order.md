# Core removal order

Read-only survey. Source verified in `/home/inanna/dev/tidepool-jev` (branch
`feat/jev-effect`, a worktree with `engine/stg-production-cutover` merged in).
Not built, not tested, nothing committed. Not checked against
`/home/inanna/dev/tidepool` or `/home/inanna/dev/tidepool-astra`.

## State-of-play correction

`plans/README.md`, `plans/stg-completion.md`, `plans/stg-production-cutover.md`
and `plans/handoff/*` describe prepared-STG as **not yet default** and Core
deletion as unstarted ("step 4/5... remain unfinished"). Source has moved past
those docs without updating them:

- `27137a928` **"the prepared-STG engine is the default route"**:
  `EngineKind::from_env()` (`tidepool-runtime/src/session/persistent.rs:81-93`)
  returns `Prepared` unless `TIDEPOOL_ENGINE=core` opts out. This is the one
  engine selector in the workspace; no Cargo feature flag exists anywhere
  (checked every crate's `Cargo.toml` for `[features]`: none).
- `67e0ffdac` **"delete Core-only session facilities without prepared
  callers"** already removed 3,540 lines, including all of
  `tidepool-runtime/tests/tenure_resume_gc_repro.rs` (2,783 lines) — step 5
  deletion is in progress, not unstarted.
- `STG_KNOWN_ISSUES.md:1-6` (repo root, not `plans/`) is the current living
  status doc: *"Prepared is the default route; `TIDEPOOL_ENGINE=core` selects
  the old engine."*
- One specific gap the stale handoff doc still flags —
  `ResidentSession::close_realm`/`parked_realm` "resolve only the Core
  engine" — is also fixed: `tidepool-runtime/src/session/persistent.rs`'s
  `ResidentEngine::close_realm`/`parked_realm` (~line 217-230) dispatch both
  `Self::Core(machine) => machine.close_realm(...)` and
  `Self::Prepared(engine) => engine.close_realm(...)` (verified by direct
  read). `tidepool-actor/tests/placement_retirement.rs`'s own header now
  documents this as fixed, dual-engine-tested behavior.

This does **not** mean Core is safe to delete. It means the blocker moved
from "Core is default" to "Core is still a required, tested, load-bearing
fallback with its own artifact format baked into the compile cache" — see
§3.

**Caveat on "retired Core evaluator" language:** several scripts/docs
(`scripts/fixtures.sh:70`, `plans/stg-production-cutover.md`) say the Core
"evaluator"/"interpreter" is retired: *"Both Rust interpreters and their
differential-test machinery have been removed."* That refers to an earlier
tree-walking interpreter generation, already gone from this tree — **not**
the Cranelift-backed Core JIT (`JitEffectMachine`,
`tidepool-codegen/src/{jit_machine,emit}.rs`), which is very much present
and is what this document calls "Core."

## 1. Classification

Legend: **C** Core-only, **S** shared, **P** prepared-only. Line counts are
`wc -l`. Corrections below are flagged where two independent passes over
this survey disagreed and a direct read settled it.

### `tidepool-repr` (Core IR + prepared schema, both live here)

| File | Lines | Class | Evidence |
|---|---:|---|---|
| `execution_schema.rs` + `execution_schema/{decode,link,codec,validation,testing}.rs` | ~6,527 | P | Zero `CoreExpr`/`CoreFrame` references in any file (grepped every file) |
| `frame.rs` | 156 | C | Defines `CoreFrame` itself |
| `free_vars.rs` | 506 | C | Doc: "a single forward pass over every node of a `CoreExpr`" |
| `normalize.rs` | 1,316 | C | `CoreExpr -> CoreExpr`; consumers are codegen/effect_machine/heap_bridge |
| `subst.rs`, `builder.rs`, `pretty.rs`, `varid_check.rs` | 715, 74, 418, 246 | C (probable, not individually read) | Only referenced from the `CoreExpr`-consuming file set |
| `serial/{mod,read,write}.rs` | 757, 1,071, 344 | C (probable, not individually read) | CBOR (de)serialization of `CoreExpr` specifically, separate from `execution_schema`'s own codec |
| `tree.rs` (`RecursiveTree<F>`) | 529 | C in practice, generic in principle | **Uncertain**: not verified whether any type besides `CoreFrame` instantiates `RecursiveTree` |
| `datacon_table.rs`, `datacon.rs`, `types.rs` | 1,181, 39, 700 | S | Constructor-metadata table and `DataConId`/`Literal`, used by `frame.rs` (C) and prepared descriptor code in `tidepool-codegen` |
| `id_issuer.rs`, `jsonl.rs`, `session_ids.rs`, `version_ladder.rs`, `actor_path.rs`, `freer_names.rs`, `trivial_field.rs` | small | S | General mechanisms per root `CLAUDE.md` mechanism index; not `CoreExpr`-coupled at the type level |

No file in `execution_schema*` imports `CoreExpr`/`CoreFrame` — the prepared
schema has no compile-time dependency on Core IR types in this crate.

### `tidepool-codegen` (Cranelift compiler + effect machine, 76,930 lines in `src/`)

| File(s) | Lines | Class | Evidence |
|---|---:|---|---|
| `jit_machine.rs` | 4,591 | C | The Core JIT machine; imports `CoreExpr` directly, uses `crate::emit::*` |
| `pipeline.rs` | 950 | C-flavored, mostly shared scaffolding | 3 explicit Core-only call sites (`LitWrapperIds`/`crate::emit` glue at lines ~131, 233, 572); the rest is shared IR-copy machinery `prepared_program.rs`/`entry_abi.rs` also use. `STG_KNOWN_ISSUES.md:88-91` notes test builds of this file inflate compile-time numbers for both routes. Delete only the 3 Core call sites in step 3, keep the rest. |
| `lower.rs` | 568 | C | 75 `CoreExpr`/`CoreFrame` refs, 0 prepared refs |
| `emit/{mod,expr,primop,case,apply,join}.rs` | 8,335 total | C | Only imported from `jit_machine.rs`/`pipeline.rs`/`binding_table.rs`; never from `prepared_program/*.rs`. Lowers `CoreExpr` to Cranelift IR |
| `datacon_env.rs` | 343 | C | 29 Core refs, 0 prepared |
| `debug.rs` | 466 | C | Only used by pipeline/effect_machine/jit_machine |
| `signal_safety.rs` | 489 | C | Only used by `jit_machine.rs` |
| `yield_type.rs` | 105 | C | Used by effect_machine/emit::case/jit_machine |
| `effect_machine.rs` | 883 | C | Confirmed by the project's own nextest battery filter (`.config/nextest.toml:19-30`, see §1 CLI table below), which lists `effect_machine` among the explicitly Core-only codegen library modules |
| `nursery.rs` | 116 | C | Same nextest-filter confirmation as `effect_machine.rs` — this resolves the "uncertain shared edge into `heap_bridge.rs`" ambiguity two independent research passes could not settle by grep alone: the project's own test tooling classifies it Core-only |
| `prepared_program.rs` + `prepared_program/**/*.rs` (~44-50 files, incl. `machine.rs` 8,027 lines) | ~40,000-48,000 (two counts from independent passes; not reconciled to the file) | P | Zero `CoreExpr`/`CoreFrame` hits anywhere under this directory. Includes `apply.rs` (1,159), `emit.rs` (1,893), `answer.rs`, `observe.rs`, `interner.rs`, `plan.rs`, byte-array/primitive files |
| `descriptor_bridge.rs` | 522 | P | No Core references found; used by `old_space/prepared.rs` |
| `old_space/prepared.rs` | 795 | P | Doc explicitly contrasts itself with "the legacy Core scanner" |
| `prepared_control.rs` | 52 | P | Single caller is `tidepool-codegen/tests/prepared_control.rs` |
| `entry_abi.rs` | 502 | P, mostly | Used only from `prepared_program.rs`/`prepared_program/{apply,emit,admission,adapter,plan,caller_result_tests}.rs` and `pipeline.rs`; no Core caller found; imports `execution_schema` types |
| `old_space.rs` | 1,571 | **S — corrected from an earlier "Core-only" guess** | Doc frames it as "gen-1 tenuring for the persistent binding store" (Core-flavored language), but `prepared_program/{invocation,forcing,roots,machine}.rs` import `crate::old_space::{OldSpace, RootSlot, PreparedCompactionStats}` directly (verified by grep + read of `old_space.rs:111,255`). `RootSlot` is also imported into `tidepool-runtime/src/session/persistent.rs`. This file cannot be deleted with Core; only its doc framing is Core-oriented, not its actual callers. |
| `machine_state.rs` | 4,778 | S | Used by both `jit_machine.rs` and 20+ `prepared_program` files; doc is engine-generic |
| `host_fns/{mod,force,primops,errors,gc,cancel,list_materialize}.rs` | ~5,600-5,612 | S (production code); `host_fns::errors` and `host_fns::primops` lib **unit tests** specifically are Core-only per the nextest filter | Production functions called from both `jit_machine.rs` and 32 `prepared_program/*.rs` files; but `.config/nextest.toml`'s battery filter separately excludes the `host_fns::errors`/`host_fns::primops` **test modules** as Core-only, meaning their inline unit tests exercise only the Core route even though the functions themselves are shared |
| `heap_bridge.rs` | 967 | S (production code); its **lib unit tests** are Core-only per the same nextest filter | Root `CLAUDE.md` mechanism index lists `tidepool-codegen::heap_bridge` with no engine qualifier (shared mechanism); 1 Core caller, 4 prepared callers by grep — but its unit-test module is in the nextest Core-exclusion list, same nuance as `host_fns` above |
| `resource_ledger.rs`, `suspension.rs`, `heap_bridge.rs`-adjacent, `descriptor_bridge.rs`-adjacent, `context.rs`, `alloc.rs`, `layout.rs`, `binding_table.rs`, `scope.rs`, `stack_map.rs`, `gc/frame_walker.rs` | ~4,900 combined | S | Each has both a Core and a prepared caller; `alloc.rs` is genuinely mixed in one file (`gc_trigger_signature` generic, `emit_prepared_failure_return` prepared-specific); `gc/frame_walker.rs` initially misclassified prepared-only by one pass, corrected to shared — it appears in the shared `host_fns` caller list too |

Important nuance found only by reading `.config/nextest.toml` directly (see
CLI table below): a file being **shared production code** does not mean its
**inline `#[cfg(test)]` module** is engine-neutral. `heap_bridge`,
`host_fns::errors`, `host_fns::primops`, `nursery`, `effect_machine`,
`jit_machine`, `emit`, `lower`, `datacon_env` all have their lib-test modules
explicitly named in the project's own Core-exclusion filter — treat the
*production* code and the *inline unit tests* as separate classification
questions for these files specifically.

### `tidepool-heap` (JIT heap layout, copying GC — 6,559 lines)

| File | Lines | Class | Evidence |
|---|---:|---|---|
| `execution_descriptor.rs`, `static_region.rs`, `descriptor_region.rs` | 924, 401, 304 | P | Docs tie directly to the prepared descriptor/`execution_schema` model; no production caller from `jit_machine.rs`/`heap_bridge.rs` found (one cross-engine *test* inside `jit_machine.rs`'s test module imports `execution_descriptor` for a `late_prepared_collection_failure_...` scenario — that is test-only, not a production dependency) |
| `gc/raw.rs` | 3,449 | **P — corrected from an earlier "shared, unverified" guess** | Read directly: doc is "Cheney's semi-space copying GC for raw HeapObjects"; every import (`descriptor_region`, `execution_descriptor`, `managed_reference`) is prepared-only. This is the prepared bump-region collector, not a collector shared with Core. `DescriptorSpace`/`OwnersMark` are prepared-specific types. |
| `gc/promotion.rs` | 707 | P (probable, ties directly to `raw.rs`) | Not individually read |
| `managed_reference.rs`, `external_storage.rs` | 176, 186 | S | Generic reference-tagging / payload-view primitives |
| `layout.rs` | 394 | S | Used directly by both `heap_bridge.rs` (S) and `jit_machine.rs` (C) per grep (`tidepool_heap::layout::write_header` at `heap_bridge.rs:925`; `tidepool_heap::layout::{...}` at `jit_machine.rs:4214,4368,4480`) |

**Correction:** an earlier pass of this survey classified `tidepool-heap` as
"entirely shared, 2,401 lines" — that undercounts the crate by more than
4,000 lines and misses that roughly 5,780 of its 6,559 lines
(`execution_descriptor.rs` + `static_region.rs` + `descriptor_region.rs` +
`gc/raw.rs` + `gc/promotion.rs`, ~88%) are prepared-only, not shared. Only
`layout.rs`, `managed_reference.rs`, `external_storage.rs` (~756 lines, ~12%)
are genuinely shared. This crate's own integration tests
(`tests/gc_unit.rs`, `tests/raw_scan_validation.rs`) have zero
Core/prepared engine markers, so they need no porting regardless.

### `tidepool-optimize` (whole crate — dead code, unrelated to routing)

**Core-only, and currently dead.** Crate charter
(`tidepool-optimize/CLAUDE.md:1-5`): *"Core-to-Core optimization passes...
Does NOT belong: evaluation."* 1,645 lines. It is a workspace member
(`Cargo.toml:13,65`) and a declared (unused) dependency of
`tidepool-runtime` (`tidepool-runtime/Cargo.toml:39`), but
`rg -rln tidepool_optimize --include='*.rs' .` finds **zero** production
call sites — the only consumer anywhere in the tree is its own
`tidepool-optimize/tests/stack_safety.rs` (verified directly). This is the
single safest, highest-confidence deletion candidate in the whole survey: it
is Core-only *and* already unreachable from any compile path, so removing
it does not even wait on the Core-vs-prepared question — see step 0.

### `tidepool-repl/` (directory, not a Cargo member)

Root `CLAUDE.md`: "currently not a Cargo workspace member — its `Cargo.toml`
was removed under the in-progress STG cutover; directory and source remain
on disk." Verified: `tidepool-repl/Cargo.toml` does not exist, and it is not
listed in root `Cargo.toml`'s `members`. `plans/stg-production-cutover.md`'s
accepted destination explicitly calls for retiring "the one-shot and REPL MCP
endpoints." Cheapest possible deletion (no build entanglement to break) —
see step 0. Not individually inventoried file-by-file in this pass.

### `tidepool-runtime/src/session/*` (turn/workbench/registry/supervisor)

| File | Lines | Class | Evidence |
|---|---:|---|---|
| `persistent.rs` | ~2,356-2,470 (two counts, not reconciled) | S — **the fork point itself** | Defines `EngineKind`/`ResidentEngine` (lines 71-160ish); both `Core`/`Prepared` match arms present throughout (`close_realm`/`parked_realm`/`residency`/etc.) |
| `resident.rs` | ~4,058-4,900+ (counts disagree between passes) | S, with 5 confirmed Core-only leaf functions | `ResidentSession`; `close_realm` (~line 1405/1493) delegates through `ResidentEngine`, dispatching to both engines (fixed gap, see State-of-play). But: |
| `resident.rs:2505` (`run_transient_with_sites`) | — | **C, live blocker** | `engine.require_core()?`, no prepared arm |
| `resident.rs:2625` (`run_binding_with_sites`) | — | **C, live blocker** | same |
| `resident.rs:2741` (`run_projected_bind_with_sites`) | — | **C, live blocker** | same |
| `resident.rs:3081` (`run_rooted_fragment`) | — | **C, live blocker** | same |
| `resident.rs:3174` (`reenter`) | — | **C, live blocker** | same |
| `prepared.rs` | 3,113 | P | 40 prepared refs vs. 6 mostly-comment Core refs; `PreparedEngine`, wraps `tidepool-codegen::prepared_program` |
| `turn.rs`, `workbench.rs`, `registry.rs`, `render.rs`, `inspection.rs`, `binding_table.rs`, `view.rs`, `facade.rs`, `recovery.rs`, `kernel.rs`, `supervisor.rs`, `dialect.rs`, `mod.rs` | ~14,300 combined | S | Zero direct `JitEffectMachine`/`PreparedEngine`/`PreparedMachine` symbol hits in any of these files; root `CLAUDE.md` mechanism index names `session::turn`, `session::workbench`, `session::registry` as single, unqualified mechanisms |
| `engine.rs` | 1,599 | S, mostly | Oneshot MCP-eval message-passing types (`EngineResumeInput`/`EngineMessage`); only 4 Core refs, 0 prepared refs — likely a small Core-specific corner in otherwise shared plumbing, not fully read |

Callers of `EngineKind::from_env()` (all follow the flipped default, none
hardcode Core in production): `tidepool-harness/src/harness.rs:938-941`
(comment: "the prepared machine is the default; `TIDEPOOL_ENGINE=core`
opts..."), `tidepool-harness/src/selfharness/driver/lifecycle.rs:220`,
`tidepool-runtime/src/session/resident.rs:1019,1090`, `turn.rs:437`.

### `tidepool-actor`

- `mount.rs:255` — the only `ActorRunTarget` impl is `impl<H,O>
  ActorRunTarget for ResidentSession<H,O>` (S — one impl, dispatches
  internally, no separate Core impl exists).
- `tests/placement_retirement.rs` — dual-engine test; its documented
  `close_realm`/`parked_realm` Core-only-resolution gap from
  `plans/handoff/continuation-2026-09-15.md` is already fixed (see
  State-of-play correction).

### `tidepool-harness`

`harness.rs:938-941` confirms the harness composition root already follows
the flipped default; no separate Core-only bootstrap remains here contrary
to the stale handoff doc's "Shoal's Core-only bootstrap remains a
default-routing dependency" claim — that claim was not independently
re-verified against current Shoal composition-root source beyond this
comment, so treat as probable-but-not-exhaustively-checked.

### Haskell (`haskell/`)

| File | Lines | Class | Evidence |
|---|---:|---|---|
| `src/Tidepool/Translate.hs` | 2,705 | **C, live blocker** | GHC Core → `CoreExpr` CBOR translator. Per `STG_KNOWN_ISSUES.md:143-154`, this is also where Core intercepts `eitherDecodeValue` to the `JsonDecode` primop — a Core-specific special case with no prepared analogue (prepared instead runs a real Haskell parser, per the same doc) |
| `src/Tidepool/Artifacts.hs` | 299 | Mixed, mostly C | `writeWholeModuleClosed`/`runMultiTargetClosed`/`translateTargetClosed` (Core-write helpers, lines 60,277,288) call into `Translate.hs`; `renderAsksJson`/site plumbing (shared) stays |
| `app/Main.hs:716` | — | **C, live blocker call site** | `writeWholeModuleClosed` runs on every compiled turn **unconditionally**, regardless of `requestPreparedTurn` — verified by direct read of the surrounding function (lines 690-720): `preparedArtifacts` is computed conditionally on `requestPreparedTurn args`, but the very next line calls `writeWholeModuleClosed` with no such guard |
| `src/Tidepool/ExecutionProjection.hs` | 1,943 | P | The STG-to-prepared-schema projector; central subject of `plans/stg-completion.md`'s execution sequence |
| `src/Tidepool/PreparedStg.hs`, `PreparedSites.hs`, `PreparedRecovery.hs` | not sized individually | P | By name and by `STG_KNOWN_ISSUES.md:10-19` (`recoverPreparedClosure` lives in `Main.hs`/`PreparedRecovery.hs`) |
| `src/Tidepool/GhcPipeline.hs` | 1,908 | S | Feeds both translators from one GHC compile; named in `plans/stg-completion.md`'s step-1 owners list for "compiler validity" applicable to both routes |
| `src/Tidepool/ExecutionIR.hs`, `ExecutionEncode.hs`, `CborEncode.hs` | 422, 401, 382 | **uncertain** | Names suggest prepared-schema IR/encoder; not individually grepped for Core coupling |
| `test-prepared-stg/*.hs` (7 files) + `execution-schema-encode`/`execution-schema-projection`/`execution-corpus-projection`/`prepared-stg-pipeline-test` cabal suites | — | P | Entirely prepared-only |
| `test/ConstructorArityTest.hs`, `IntrospectionSearchTest.hs`, `Suite.hs`, `TextSuite.hs`, `Identity.hs`, `SibDict.hs`, `test-cell-splitter`, `test-display-tree` | — | S | Extractor/GHC-Core-plumbing tests (shared substrate), not the Core JIT execution route |

### CLI / config / cache

| Item | Location | Class | Note |
|---|---|---|---|
| `TIDEPOOL_ENGINE` env var | `tidepool-runtime/src/session/persistent.rs:81` | routing switch | `core` opts out; unset/`prepared`/anything else → prepared (default since `27137a928`). This is exactly what step 5 of `plans/stg-completion.md` deletes "with the Core engine" |
| `TIDEPOOL_CORE_TESTS` env var | `scripts/lib-extract.sh:154-163`, `scripts/battery.sh:73`, `scripts/battery-shard.sh:66` | Core-only test gate | `TIDEPOOL_CORE_TESTS=1` makes the battery scripts run the excluded Core-engine tests instead of skipping them (verified directly) |
| `[profile.default]`/`[profile.battery]` `default-filter` | `.config/nextest.toml:19-30` | **the authoritative Core-only test-exclusion list** | Comment (verified verbatim): *"Core-engine tests are excluded from every tier: the Core JIT is being replaced by prepared STG and its tests only cost time. The Core set is the Core-JIT codegen suites (codegen, gc, resident, properties), the Core-only codegen library modules, the Core optimizer, and every `_on_core` dual-run variant."* Filter expression excludes, by name: `tidepool-codegen` binaries `codegen`/`gc`/`resident`/`properties`; `kind(lib)` test modules matching `/^(jit_machine\|emit\|lower\|nursery\|datacon_env\|heap_bridge\|effect_machine\|host_fns::errors\|host_fns::primops)::/`; all of `package(tidepool-optimize)`; and `test(/_on_core$/)`. This is a stronger, more precise source than grep-based classification for exactly these modules — used above to resolve `nursery.rs`/`effect_machine.rs` uncertainty. |
| `--all-closed` extractor flag | `tidepool-extract-cmd/src/request.rs:114,455`; consumed `haskell/app/Main.hs:~420-429` | C | Full-module Core CBOR dump, used by `scripts/fixtures.sh`'s corpus regeneration |
| `--prepared-turn` extractor flag | `tidepool-extract-cmd/src/request.rs:511` | P | |
| `--dump-core` extractor flag | `tidepool-extract-cmd/src/request.rs:50,113,454,730` | C, likely **retained** | Introspection dump; root `CLAUDE.md` step 5 language: "retain Core work still needed for... introspection" |
| `TIDEPOOL_TEST_DROP_DC` | `haskell/src/Tidepool/Artifacts.hs:155` (approx) | S | Per `plans/handoff/continuation-2026-09-15.md:259-261`, the check applies to "either a Core-emitted or prepared-admitted constructor" — not Core-only |
| Artifact cache | `tidepool-toolchain/src/cache.rs:277-394` | **S, unified format, blocker** | `CachedArtifactParts` is a hardcoded 4-tuple `(expr, meta, asks, prepared)`; `cache_store`/`cache_load` write/read one entry with all four, one sentinel (`blake3(...) || blake3(prepared_bytes)`, 128 raw bytes). A cache entry with only one artifact kind is treated as absent. Cannot drop the Core third without a cache-format version bump. |
| `CompiledTarget` | `tidepool-toolchain/src/artifacts.rs:271-291` | **S, unified format, blocker** | `pub expr: CoreExpr` field; comment already says "Per-target legacy Core + prepared program + asks sidecar" |
| No `[features]` in any workspace `Cargo.toml` | checked `tidepool-codegen`, `-runtime`, `-heap`, `-repr`, `-toolchain`, `-actor`, `-harness` | — | Confirms `TIDEPOOL_ENGINE` is the only gate; the Core/prepared split is at the module/crate level, not compile-time features |
| `justfile` / `scripts/*.sh` | repo root | — | No Core-only recipe or script remains; the old differential/comparison machinery is already gone (`scripts/fixtures.sh:70` comment references "the retired Core evaluator" — see the caveat above about what that phrase actually refers to) |
| `tidepool/src/bin/*.rs` (facade CLIs) | `shoal.rs`, `tidepool-compile-report.rs`, `tidepool-selfharness.rs`, `prompt_catalog.rs` | — | Zero Core references; engine choice is below the CLI layer |

## 2. Blockers to deletion (evidence-backed, current — not the stale plan-doc list)

1. **Core artifact generation is unconditional per turn.** `haskell/app/Main.hs:716`
   calls `writeWholeModuleClosed` (→ `Tidepool.Translate`, 2,705 lines) on
   every compiled turn whether or not `requestPreparedTurn` is set.
   `STG_KNOWN_ISSUES.md:58-63` documents this as a known, undesigned gap:
   *"Prepared requests still build the Core artifact... Direction: derive
   those from the prepared program and skip Core emission on prepared
   requests. Architectural; not designed yet."* Blocks: deleting
   `Translate.hs`, `Artifacts.hs`'s Core-writer functions, and
   `tidepool-repr`'s `CoreExpr` serialization path.
2. **The artifact cache and `CompiledTarget` are one unified quad format**
   (`tidepool-toolchain/src/cache.rs`, `artifacts.rs`). Removing the Core
   third of the tuple is a cache-format/version change, not a code deletion
   — every cache consumer and the sentinel layout must be touched together.
3. **Five `tidepool-runtime/src/session/resident.rs` functions have no
   prepared arm** (`run_transient_with_sites:2505`, `run_binding_with_sites:2625`,
   `run_projected_bind_with_sites:2741`, `run_rooted_fragment:3081`,
   `reenter:3174`), each calling `engine.require_core()?`. These implement
   handle/framed-answer and rooted-fragment turn shapes that
   `plans/handoff/continuation-2026-09-15.md`'s F5 section lists as not
   landed (handle/framed answers, Either/list wires, some ordinary-effect
   reply sites). `TIDEPOOL_ENGINE=core` is not just a test knob — it is the
   only way to reach these code paths in production today, and it is a
   supported, tested opt-out (`TIDEPOOL_CORE_TESTS=1` runs the whole
   `_on_core` suite against it).
4. **Effect/answer coverage gaps push real traffic to Core**, per
   `STG_KNOWN_ISSUES.md`: `LiveReentry` deliveries have no prepared-route
   producer yet (lines 156-159); representation-polymorphic
   caller-chosen-result functions compile once per demanded shape rather
   than universally (lines 80-86); ordinary effects (`say`, file, KV, HTTP,
   form `ask`) rely on synthetic sites rather than dynamic ones (lines
   95-98). Programs exercising these paths may still need Core even though
   the session-level engine choice itself never silently falls back
   (`persistent.rs`'s doc: "never falls back to Core after a turn starts or
   fails").
5. **Residency/lifetime work on the prepared machine is incomplete**
   (`STG_KNOWN_ISSUES.md` "Residency" section): external payloads not swept
   from old space, nursery/old-space retirement cycles deferred and
   untested, shadowed bindings pin their programs indefinitely. Blocks
   calling prepared "at parity," the stated precondition
   (`plans/stg-completion.md` step 4) before a confident Core removal — not
   independently re-verified against current source beyond this doc.
6. **`old_space.rs` (`tidepool-codegen`, 1,571 lines) is genuinely shared**,
   not Core-only despite its "persistent binding store" framing —
   `OldSpace`/`RootSlot`/`PreparedCompactionStats` are used directly by
   `prepared_program::{invocation,forcing,roots,machine}.rs`. It cannot be
   deleted wholesale in an early "leaves first" pass the way `jit_machine.rs`
   can.
7. **`plans/stg-completion.md`'s own step sequence puts default-routing
   (step 4) before deletion (step 5)**, and step 4's exit criteria (pinned-
   GHC oracle under the real notebook dialect, the full production corpus
   through projection/validation/compile/execution/comparison, passing
   retained broad gates) were **not verified in this pass** — the default
   flip (item above) is necessary but the plan's own text does not treat it
   as sufficient. This survey performed no builds and cannot resolve
   whether step 4 is actually done; treat as the single largest open
   question.
8. **`plans/README.md`, `stg-completion.md`, `stg-production-cutover.md` are
   stale generally** (see State-of-play correction): plan removal order off
   `STG_KNOWN_ISSUES.md` and direct source, not those docs.

## 3. Dependency-ordered removal (leaves first)

Ordering rule: a step is safe to land, and leaves the workspace compiling,
only once everything with an incoming reference to that step's targets has
either been ported to the prepared equivalent or deleted alongside it. Steps
0-2 do not depend on the blockers in §2 at all and can happen today; step 3
onward is gated on closing §2.

| Step | What | Files (approx lines) | Callers to touch | Tests most likely to break |
|---|---|---|---|---|
| **0. Dead weight, zero dependency wait** | `tidepool-optimize` crate (whole, unreachable already); `tidepool-repl/` directory (not a Cargo member already) | 1,645 + repl dir | Remove `tidepool-optimize` from root `Cargo.toml`'s `members`/`workspace.dependencies` (lines 13, 65) and `tidepool-runtime/Cargo.toml:39`'s unused dep; `rm -rf tidepool-repl` (no build entanglement) | `tidepool-optimize/tests/stack_safety.rs` (deleted with the crate, its only consumer); verify no doc-generation step reads `tidepool-repl/` first |
| **1. Battery-excluded Core-only test files** | `tidepool-codegen/tests/*.rs` files driving `jit_run::compile_and_run`/Core `CoreExpr` fixtures directly — the `codegen`/`gc`/`resident`/`properties` binaries named in `.config/nextest.toml`'s own filter, plus files with no engine marker that use the same `session_scaffold` Core-compile harness (57-64 of ~70-81 files across independent counts; roughly 24,500-28,000 lines) | ~24,500-28,000 | Each file is registered via a `mod` line in `tidepool-codegen/tests/suites/{codegen,gc,properties,resident}.rs` — deletion needs the matching registration removed too, or `just suite-check` fails (root `CLAUDE.md`: suite entry points must cover every top-level test file) | `just suite-check`; `just suite tidepool-codegen`; but see the porting caveat below — much of the `gc`/`resident` content tests the shared `tidepool-heap` collector through Core-compiled code and should be **ported**, not deleted (§4) |
| **2. `_on_core`-suffixed parity tests** | 30 functions across 5 files: `tidepool-actor/src/resident_workbench.rs:7114`; `tidepool-actor/tests/placement_retirement.rs:468`; `tidepool-runtime/src/session/persistent.rs:2346,2380,2412`; `tidepool-runtime/tests/session_scope_retirement.rs` (10 functions, lines 123,166,201,232,333,380,401,422,475,560); `tidepool-runtime/tests/prepared_turn.rs` (15 functions, lines 419,596,862,1005,1176,1289,1443,1537,1703,1808,1916,2048,2151,2292,2370) | small (function bodies only, maybe 800-1,200 total) | None outside the test files themselves — the naming convention (introduced alongside `27137a928`) exists precisely because each has a same-behavior, un-suffixed prepared sibling already in the same file that stays long-term | Verify all 30 actually have a live un-suffixed sibling before treating this as pure deletion — spot-checked for `prepared_turn.rs`'s pattern (explicit pairing visible in-file), not individually confirmed for all 30 |
| **3. Core-JIT emission layer (blocked on §2)** | `tidepool-codegen/src/{emit/{mod,expr,primop,case,apply,join},jit_machine,lower,datacon_env,debug,signal_safety,yield_type,effect_machine,nursery}.rs` | ~15,900 | The 3 Core-only call sites in `pipeline.rs` (~131,233,572); `binding_table.rs`'s `crate::emit::ExternalEnv` import; `tidepool-runtime/src/session/persistent.rs`'s `ResidentEngine::Core` variant and every `require_core`/`core_mut`/`.core()` call site; `EngineKind`/`TIDEPOOL_ENGINE` itself (step 6 below, or fold in here) | Everything from step 1 not already deleted; the exact lib-test module names in the nextest filter (`jit_machine`, `emit`, `lower`, `nursery`, `datacon_env`, `heap_bridge`, `effect_machine`, `host_fns::errors`, `host_fns::primops` — note the last three are shared *files* whose *test modules* need separate handling, not blanket deletion) |
| **4. Delete the 5 `resident.rs` Core-only functions** (now dead once no `EngineKind::Core` construction remains) | `run_transient_with_sites`, `run_binding_with_sites`, `run_projected_bind_with_sites`, `run_rooted_fragment`, `reenter` | not separately counted (part of a ~4,000+ line file) | `mount.rs`'s `ActorRunTarget` impl (already engine-neutral, likely untouched) | `resident_session.rs`, `session_scope_retirement.rs`'s remaining `_on_core` bodies if step 2 didn't already remove them |
| **5. `tidepool-repr` Core IR** | `frame.rs` (156), `free_vars.rs` (506), `normalize.rs` (1,316), `subst.rs`/`builder.rs`/`pretty.rs`/`varid_check.rs` (1,453), `serial/*` (2,172), `tree.rs` if nothing else instantiates `RecursiveTree` (529, **verify first**) | ~6,100-6,600 | Nothing outside `CoreExpr`-consuming files was found importing it — checked clean in `execution_schema*`, `prepared_program/*`; `tidepool-toolchain/src/artifacts.rs:39,276` (`CompiledTarget.expr: CoreExpr`) must already be gone via step 7 | `tidepool-repr` unit tests referencing `CoreExpr`; `golden_wire_contract.rs`, `stack_safety.rs` in `tidepool-repr/tests/` (**unverified** whether these test the Core wire format specifically or `execution_schema`'s) |
| **6. Haskell: delete `Translate.hs` (2,705) and `Artifacts.hs`'s Core-writing functions** (`writeWholeModuleClosed`, `runMultiTargetClosed`, `translateTargetClosed`; keep `renderAsksJson` and site/metadata plumbing) | ~2,900 | `Main.hs:420-441,599-608,716`; drop `--all-closed` CLI flag from `tidepool-extract-cmd/src/request.rs`; keep `--dump-core` if introspection still needs it | `scripts/fixtures.sh` (stop requesting `--all-closed`); any cabal test asserting on `.cbor` Core artifact shape; `proptest_cache_layer` (already `MissingOutput` for prepared-only fake-extractor output per `plans/stg-completion.md:231-236` — this is a live, pre-existing gate break to reconcile, not a new one this step introduces) |
| **7. Retire `TIDEPOOL_ENGINE`/`EngineKind`/`ResidentEngine`, and shrink `cache.rs`/`artifacts.rs` from a 4-tuple to a 3-tuple** | small (`persistent.rs`'s enum + accessors, ~100-200 in the toolchain crate) | every construction site listed in step 3's callers column; every `cache_load`/`cache_store` call site | `legacy_cache_sentinel_misses_after_prepared_cutover` (already tests this exact transition — keep the test, update its assertion) |
| **8. Port GC/realm test suites** in `tidepool-codegen/tests/` (`gc`, `resident` binaries — dozens of files, several hundred to 1,000+ lines each: `array_gc_safety`, `gc_write_barrier`, `realm_*`, `continuation_gc_root`, `stackmap_*`, `con_midfill_gc_safety`, `apply_cont_heap_composition_gc`) from Core-compiled fixtures to prepared-compiled fixtures | not a deletion — a rewrite; budget this as the largest test-porting cost in the whole removal | `tidepool-codegen/tests/` `session_scaffold` support harness (currently built on Core compile) | all Core-route GC/heap-safety tests; `tidepool-heap`'s own GC tests already have zero engine markers and need no porting, so this step is specifically about the codegen-layer integration tests that exercise the shared heap mechanism only through Core-compiled entry points today |

Order rationale: steps 0-2 are genuinely free (dead code, already-excluded
tests, deliberately-paired test twins) and total roughly 27,000-30,000
lines with zero production-behavior risk. Step 3 is the actual load-bearing
Core Cranelift compiler (~15,900 lines) and is where §2's blockers bite —
it cannot land until blocker 3 (the 5 `resident.rs` functions) is closed,
because those functions are real production callers of the Core engine.
Step 5 (`tidepool-repr`) must follow step 3 because `jit_machine.rs`/`emit/*`
are the only consumers of `normalize.rs`/`free_vars.rs`. Step 6 (Haskell) is
gated on blocker 1 specifically (unconditional `writeWholeModuleClosed`) and
can in principle start in parallel with steps 3-5 once that guard is added.
Step 8 is listed last because it is the most expensive and least mechanical;
in practice it should start as soon as prepared has GC/realm parity, not
wait for every prior step to land.

**Not sized in this pass, flagged for a follow-up survey:** `tidepool-bridge`,
`tidepool-agent`, most of `tidepool-web`, and per-file granularity inside the
~14,300 shared `tidepool-runtime/session/{turn,workbench,registry,...}.rs`
lines (classified whole-file only, not audited for internal Core-only
branches).

## 4. Tests: delete vs port

### Delete wholesale (Core-JIT-compiler-internals only, no shared-mechanism content)

| File(s) | Evidence |
|---|---|
| `tidepool-runtime/tests/resident_session.rs` | Module doc: "Pinned to `EngineKind::Core`: this whole suite drives `compile_session`/`add_function`/suspend-resume mechanics the prepared engine does not share." |
| `tidepool-runtime/tests/green_thread_representation.rs` | Same pattern, explicit pin comment |
| `tidepool-runtime/tests/tenure_resume_gc_repro.rs` | **Already deleted** in `67e0ffdac` (2,783 lines) — precedent, not remaining work |
| `tidepool-runtime/tests/captured_real_core.rs` and its `captured_core/` fixture directory | Name states Core-specific capture directly |
| `tidepool-runtime/tests/cross_mode_{tests,existing,targeted}.rs` + `cross_mode_harness/` (~1,300+ lines) | **Verified by direct read**: despite the "cross_mode" name suggesting Core-vs-prepared parity, `cross_mode_harness/mod.rs:1-31` shows it tests single-module vs. multi-module GHC Core compilation shape equivalence (`CoreExpr fields at lines 8,28-31`), guarding against Core divergence across module splits (regression for a specific past PR). This is Core-only, not an engine-parity harness. **Open design question, not resolved here:** does the prepared path (`ExecutionProjection.hs`) have an equivalent single-vs-split-module risk that needs its own new test before this one is deleted, or is the risk specific to Core's translation shape? Needs a design answer, not a mechanical port. |
| All 30 `*_on_core` test functions (§3 step 2) | Named and excluded from nextest's default filter specifically because they drive Core-only machinery; each has a same-behavior prepared sibling already |
| `tidepool-optimize/tests/stack_safety.rs` | Deleted with the crate in step 0 |
| Core-codegen-correctness-only files in `tidepool-codegen/tests/` with no GC/heap content (e.g. `emit_expr.rs` 3,298 lines, `emit_case.rs`, `emit_join*.rs`, `tco*.rs`, `apply_acceptance.rs`, `realm_multi_continuation.rs` 1,553, `proptest_host_arrays.rs` 1,321, `boxed_array_behavior.rs` 1,060, `effect_machine.rs` 1,091 — **file list not individually verified line-by-line**) | Test Cranelift emission correctness for a backend being deleted; prepared's equivalent behavior is tested via its own `prepared_program/*_tests.rs` in-module suites |

### Must be ported, not deleted (exercise engine-agnostic mechanisms via Core today)

| File(s) | Why it must survive | Evidence |
|---|---|---|
| `tidepool-codegen/tests/` `gc` and `resident` suite files (`array_gc_safety`, `gc_write_barrier`, `realm_*`, `continuation_gc_root`, `stackmap_*`, `con_midfill_gc_safety`, `apply_cont_heap_composition_gc`, etc.) | Test the shared `tidepool-heap` GC mechanism, currently reachable only by compiling through Core | `tidepool-heap`'s own tests have zero engine markers — this codegen-level suite is the only place some GC invariants are checked end-to-end |
| `tidepool-actor/tests/placement_retirement.rs` | Dual-engine test proving both engines retire placements identically; currently the load-bearing proof that the (fixed) `close_realm`/`parked_realm` gap stays closed | Its own header documents the dual-engine design |
| `tidepool-runtime/tests/prepared_turn.rs`'s `EngineKind::Core` half of each `notebook_X(EngineKind::Core)`/`notebook_X(EngineKind::Prepared)` pair | Deleting Core drops exactly the Core call and branch inside each shared test body — the Prepared half stays | Pairs at the 15 `_on_core` line numbers listed above, each next to its un-suffixed sibling |
| `tidepool-runtime/tests/proptest_jit_vs_eval.rs` | Name suggests a Core-vs-something comparison — **unverified**, read before deciding whether it deletes or ports | Not opened in this survey |
| Whatever currently exercises §2 blocker 4's gaps (`LiveReentry` delivery, representation-polymorphic result functions, ordinary-effect synthetic sites) | Presumably Core-only today by necessity (prepared can't do it yet); needs a prepared-side test once that feature work lands, not before | `STG_KNOWN_ISSUES.md` lines cited in §2 |

### Already prepared-only, unaffected

`tidepool-runtime/tests/{prepared_execution,prepared_resident_composite,prepared_reply_types,prepared_residency}.rs`;
`tidepool-codegen/tests/{prepared_control,m5_lifetime_stress}.rs`; all of
`haskell/test-prepared-stg/`; `tidepool-repr/tests/{execution_schema_codec,execution_schema_contract}.rs`.

### Fixtures

`haskell/test-prepared-stg/fixtures/*`, `fat-iface-fixtures/*`, `site-fixtures/*`
are prepared-only and stay. `tidepool-codegen/tests/fixtures/` contents were
not individually classified (out of survey budget) — check before step 1/3.
Any `.cbor` fixture regenerated only via `--all-closed` becomes dead weight
once step 6 lands.

## 5. Uncertainty not verified in this pass

- Exact line count and file count for `tidepool-codegen/src/prepared_program/**`
  and `tests/` disagreed between independent passes (~40,000 vs ~48,000
  source lines; 57 vs 64 of ~70 vs ~81 test files) — not reconciled to a
  single authoritative count.
- `tidepool-codegen/src/effect_machine.rs`/`nursery.rs`: classified Core-only
  via the nextest filter (production-strength evidence for their *test*
  modules), but whether their *production* code has any prepared-route
  caller at all (vs. zero) was not separately confirmed by reading the
  files.
- `tidepool-repr/src/tree.rs`: whether `RecursiveTree<F>` is instantiated by
  anything besides `CoreFrame`.
- `tidepool-repr/tests/{golden_wire_contract,stack_safety}.rs`: which wire
  format they golden-test (Core `CoreExpr` CBOR vs `execution_schema`).
- `tidepool-runtime/tests/proptest_jit_vs_eval.rs`: not opened; ambiguous
  between "Core-vs-Prepared parity" and something else.
- `haskell/src/Tidepool/{ExecutionIR,ExecutionEncode,CborEncode,PrimOps,Resolve,Binders,Introspection}.hs`: not individually classified.
- ~14 `tidepool-codegen/tests/` files with no grep hit on either `Core`/
  `prepared` markers (`binding_table_realm_isolation`, `deep_force_nf`,
  `e6_no_rules_pragma`, `frame_walker_hardening`, `proptest_boundary_roundtrip`,
  `proptest_heap_layout`, `proptest_host_fns`, `realm_leak_comparison`,
  `realm_module_growth`, `realm_root_growth`, `scaffold`,
  `session_seed_external_env_root_retention`, `shape_trap_dump_oob`,
  `signal_safety`) — likely Core-route via the shared `session_scaffold`
  harness, not read individually.
- `tidepool-toolchain` has no `tests/` integration directory; its unit tests
  live inline in `src/cache.rs`/`artifacts.rs`/`prepared_artifact.rs` and
  were not enumerated test-by-test.
- `tidepool-actor`, `tidepool-harness`, `tidepool-bridge`, `tidepool-agent`,
  and the `tidepool` facade crate were not surveyed at full file level
  beyond the specific hits already listed — root `CLAUDE.md`'s mechanism
  index implies most of this is engine-agnostic by design (actor lifecycle,
  mount boundary), but this is not exhaustively verified.
- §2 blocker 7 (step 4's broad-gate exit criteria) and §2's harness-bootstrap
  claim were explicitly not run/re-verified — this was a read-only survey
  with no builds or tests executed, per instructions.
