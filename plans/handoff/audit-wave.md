# Read-only audit wave (2026-09-15)

Fourteen small read-only Opus audits of the STG cutover code. Nothing here was
built or run by the auditors; every finding needs confirmation (ideally a
failing test) before a fix. Findings are recorded as they arrive, most severe
first within each area.

## Test coverage map (prepared engine)
All prepared-engine tests are hand-written; no proptest touches
`prepared_program` or `execution_schema`, and the only semantic oracle is the
GHC-oracle JSON cohorts.
- Well covered: `machine.rs` (32 inline), `primitives.rs` (27), arrays/bytes
  (~43 incl. GC survival), `addresses.rs` (12), `admission.rs` (10), sibling
  `*_tests.rs` (~100), `validation.rs` (44), `link.rs` (11), repr codec (12) and
  contract (7) tests, `prepared_execution.rs` (14), Haskell `test-prepared-stg`
  (~127 assertions), corpus cohorts (30).
- No direct tests: `emit.rs` (1760 lines), `plan.rs`, `run.rs`, `forcing.rs`,
  `fallible.rs`, `entry.rs`, `adapter.rs`, `interner.rs`, `roots.rs`,
  `codec.rs` (5 tests for 1005 lines).
- Never asserted: `ExecutionError::TopSlotBaseMismatch`, `DescriptorShape`,
  `Static`; `StaticImageError::Allocation` never triggered.
- Cancellation: each `PreparedSafepoint` kind referenced 2-5 times; only
  foreign excess calls are cancelled at every poll.
- GC during primitives missing for `text_search`, `wide_words`, `floating`,
  `md5_kernel`, `static_bytes`, `data_tag`.

Top tests to add:
1. Prepared-vs-Core differential proptest over generated closed Core
   (reuse `proptest_jit_vs_eval` generators; register in `suites/properties.rs`).
2. Codec round-trip plus byte-flip/truncation fuzz (never panic).
3. Validation soundness: single mutations of valid programs are rejected before
   compile.
4. Cancel at every safepoint index for a program hitting all five kinds; rerun
   succeeds.
5. Forced GC at every allocation inside the uncovered primitives; results equal
   the no-GC run.
6. Install-order property across programs with shared constructors, imports
   and interleaved collections; conflicting descriptors give `DescriptorShape`.
7. `TopSlotBaseMismatch` and static-image allocation failure (machine stays
   reusable).
8. `fallible`/`forcing` boundary: primitive failure at observation budget
   clears roots.

## Observation and forcing
Nothing serious: traversal is stack-safe (`try_expand_and_collapse`, 20k-deep
test), every force goes through rooted slots with the heap reader rebuilt
after each force (`forcing.rs:186-243`), cycles end in `BudgetExceeded`.
1. Medium — Value shape divergence from the Core bridge: `Char#` projects as
   `WordRep 32` so prepared yields `LitWord` (Core `LitChar`,
   `heap_bridge.rs:290`); external bytes become `Lit(LitByteArray)`
   (`observe.rs:522`) vs Core `Value::ByteArray` (`heap_bridge.rs:328/338`).
   Normalize in one place (literal-kind hint for `C#`, one byte-array shape).
2. Medium — boxed arrays, continuations, functions and PAPs are
   `Unobservable` (`observe.rs:526`); Core renders arrays as
   `Con(DataConId(0), elems)` and closures as a sentinel. Add a boxed-array arm
   charged per element.
3. Low — `inspect_constructor` (`observe.rs:349`) and `resolves_to_whnf_value`
   (`:384`) follow `Updated` indirections with no hop limit (a corrupt cycle
   hangs); cap hops with an integrity error.
4. Low — budget depends on evaluation state (each indirection hop costs a
   unit, `:455`); root-slot limit counts pending seeds (`forcing.rs:73`).
5. Low — `unreachable!` on `Void` (`:458`) should be a `Representation` error;
   missing constructor metadata reported as `InvalidRange` (`:551`).
Tests: Core-vs-prepared differential on `Char`, `Text`/`ByteArray`, boxed
`Vector`; forcing a lazy field that triggers collection plus promotion before
the next field; cyclic indirection gives a typed error; diamond sharing pins
per-path charging.

## Cancellation safepoints
Nothing serious. `FunctionEntry` polls in every compiled function
(`emit.rs:180-188`) and dispatcher (`apply.rs:715-724`); `ThunkEntry` at the
head of the indirection loop (`entry.rs:99-112`, only back edge `:175`);
`Backedge` on every join `Jump` (`emit.rs:361-366`, tested
`settlement_tests.rs` ~480); `Allocation` on the GC slow path only
(`gc.rs:1115`). No `return_call` is emitted, so endless tail loops poll each
step and end in `StackOverflow`. `ThunkCommit` polls before the commit
(`entry.rs:270-278`); failure restores the header (`:400-424`); the commit
itself has no safepoint (`:372-390`).
- Low: adding real tail calls later removes the `StackOverflow` backstop;
  pin termination on `FunctionEntry` with a test.
- Low: confirm the realm flag from `ResourceLedger` is the same `Arc` passed
  to `PreparedInvocation::enter` (`invocation.rs:141`; `prepared.rs:638`).
Test: self-tail-recursive `go n = go (n+1)` with
`fail_prepared_at(FunctionEntry, 10_000, Cancelled)` gives `Cancelled`, not
`StackOverflow`; a looping memoized thunk cancelled at the Nth `Backedge` is
Live again afterwards.

## Machine install failure paths
1. **Medium (bug)** — a failed `collect_on` during a second-program install
   (`machine.rs:593-602`) records a reusable GC cause (`gc.rs:1264`) that
   `install` never clears (only `run_entry*` call `end_prepared_call`); the
   machine reports `Reusable` but the next `install_program` fails at
   `poll_prepared`, and `retain_top` fails in `promote_prepared`
   (`old_space/prepared.rs:98`). The existing
   `t3_install_failure_after_registries_extend…` hides it by calling
   `run_entry` next. Fix: clear the pending cause on every post-`collect_on`
   install error (a scope guard). Test: t3 setup, clear the heap ceiling, then
   `retain_top` and a second install with no `run_entry` between.
2. Low — static and heap top-table writes (`machine.rs:464-478`,
   `initialize_heap_tops`) are not zeroed on error, contrary to the doc at
   :329; cells past the next program's range keep pointers into dropped
   statics. Zero `base..base+slot_count` on error.
3. Low — the descriptor and static-region union (`machine.rs:559-565`,
   `machine_state.rs:875-882`) is never undone after a later failure; each
   retry leaks a static image and GC admits pointers into an unowned region.
4. Low — `absorb` (`machine.rs:389`) and `compile_for_install` (`:314`) intern
   descriptors before install succeeds, so a failed program still claims
   constructor identities (later different declaration refused). Compile
   against a staged interner and merge on success.
Structural fix: one install transaction guard recording slot range, stack-map
push, descriptor extension, published imports and staged interner, undone on
drop, disarmed by `commit()`. Ordering between verification, publication and
`collect_on` is sound; `release`, `close_realm` and `Drop` are fine.

## Artifact trust boundary (decode, validate, link) and emission
Two audits independently found the same validator hole.
1. **Critical (memory unsafety / miscompile)** — a `Jump` in a case scrutinee
   is accepted: the scrutinee is walked with the parent's join scope
   (`validation.rs:717-722`, `:346-351`) and a join body is checked only
   against its own signature (`:980-985`), never against the result expected
   where it is bound. Codegen compiles the scrutinee with enclosing joins in
   scope (`emit.rs:296-306`, jump at `:359-379`), join bodies return to the
   function exit (`:341-348`), and `atom_value` does not check reps
   (`:1525-1527`). A function returning `[LiftedRef]` with
   `letjoin j :: () -> [Word64] = Return [w]` and body
   `Case (Jump j) [Word64] Default -> …` validates; at runtime the word reaches
   a `LiftedRef` exit and is recorded as a GC pointer, or the case alternatives
   are silently skipped. GHC never emits this; a corrupt or hand-built artifact
   can. Fix: fresh join scope for scrutinees (as closure bodies), require join
   results to satisfy the binding site's expected result, and assert plan rep
   equals expected rep in codegen.
2. Medium (DoS) — recursive groups are quadratic in the validator (every
   sibling re-applies the group's bindings, `validation.rs:647-650`,
   `906-907`, `982`; groups decode without counting entries, `codec.rs:743-747`,
   up to 2^18 bindings). Apply once or charge the work.
3. Medium (DoS) — codegen clones the in-scope value map at each case,
   alternative and join (`emit.rs:292`, `1037`, `1059`, `333`): O(depth²).
   Use a persistent map or undo log.
4. Risk — no native tail calls (`return_call` absent; all calls are
   `call` + `return_` under the Tail convention, `apply.rs:375/488/849/872/613/645`):
   long `Call`-frame recursion ends in `StackOverflow` instead of running in
   constant stack. Emit `return_call`/`return_call_indirect` for calls in return
   position (keep the `FunctionEntry` cancellation test).
5. Smells — `logical_arguments` drops arguments when physical values run short
   (`apply.rs:768-782`; should be a compile error); zero-argument PAP
   application copies the PAP (`apply.rs:510-523`); the case trap's returned
   status is replaced with a hardcoded `IntegrityFailure` (`emit.rs:1182`).
Sound: layout/root-mask/alignment canonical checks, tag bounds and family
agreement, operation name+signature match, id bounds, expression tree
ownership, GC stack-map declarations across calls, status checks after host
calls. Fuzz strategy: structured mutations of `testing::wire_program` (move a
subtree into scrutinee position, retarget jumps, change results, duplicate
recursive siblings, deepen nesting); validation passing implies compile `Ok` or
typed error under the Cranelift verifier plus the rep assert; linear-time
property; encoder round-trip of every mutant.

## PreparedRuntime session invariants
1. **High (latent)** — `close_realm_report(RealmId::ROOT)` /
   `retire_placement(ROOT, _)` (`prepared.rs:660-667`,
   `resource_ledger.rs:90`) has no ROOT guard: it deregisters every `bind_top`
   handle (all ROOT, `machine.rs:1138`) and releases all leases; the binding
   table keeps dead entries, later installs fail `UnknownPreparedHandle`, and
   `release_binding` removes the entry before failing (`:602-613`). The doc at
   `machine.rs:783` is wrong. Current callers use fresh realms. Fix: refuse or
   no-op ROOT (runtime or ledger).
2. Medium — bindings made during a placement are ROOT scope and realm
   (`prepared.rs:433`), so retirement frees none; `resolve_import`
   (`556-572`) searches all live bindings regardless of scope, letting one
   actor import another's same-identity binding. Thread `SessionRunContext`
   into `bind_top` and filter imports by scope.
3. Medium — `resolve_import` tie at one generation picks by HashMap order
   (`569-571`); an explicit `(identity, id)` pair is never checked against the
   binding's recorded identity (`500-503`). Tie-break by `SessionVarId` or
   refuse; check identity with a typed error.
4. Lower — `bind_top` retains before looking up facts (`400-415`, leaks a root
   on failure); backwards `set_val_gen` reports `GenerationNotStarted` (`373`);
   allocation failure (`755`) and non-latching `BadPointer` classify as
   `Language`; the Send comment (`303`) is stale (`ProgramFacts`); dedupe
   `leased` if a program can declare a global twice.
Tests: ROOT close keeps bindings and leases; same-generation tie; mismatched
explicit import pair; `bind_top` failure leaves handle count; post-compile
install failure leases nothing; `release_binding` after realm close;
backwards `set_val_gen`.

## Duplicated mechanisms and dead code
1. Turn imports are built from raw strings (`prepared_turn.rs:169-188`,
   `turn.rs:380`): operators are not parenthesized, `namespace` and `unit` are
   ignored, `Retained.generation` is a bare `u64` (`:73`). Let
   `execution_schema` own rendering an identity as an import item.
2. Three PrimRep-to-representation mappings in Haskell
   (`ExecutionProjection.projectRep` 1362, `PreparedFacts.representation` 138,
   `ExecutionIR.repForm`/`renderTypeReps` 352/386); inventory and projection can
   disagree. One owner (`ExecutionProjection`).
3. `returned_reps().unwrap_or(&[])` conflates "never returns" with "returns
   nothing" (`emit.rs:241,286`, `plan.rs:231`, `validation.rs:1227`); add a
   documented `physical_reps()` and match `NoSuccess` explicitly. (Also a
   prerequisite for `CallerResult`.)
4. Three `renderType` copies (`Resolve.hs:179`, `PreparedSites.hs:205`,
   `GhcPipeline.hs:1629`); `siteIdentity` hashes rendered type text, so site
   ids depend on which copy runs. One copy.
5. Redundant test gates (`invocation.rs` is test-only module-wide yet has inner
   `#[cfg(test)]`; bare `#[test]` in `arrays.rs:34`; test-only constructors in
   `observe.rs:148,193,408`); move to `*_tests.rs`.
6. Dead `include` field under `#[allow(dead_code)]` (`session/resident.rs:621`).

## Numeric and formatting intrinsic parity
1. Medium — shortest-digit double rendering (`tidepool-bignum/src/lib.rs:127`,
   `:138`, via `formatting.rs:63`) uses Rust's search, which admits the
   rounding boundary for even mantissas; GHC's `floatToDigits` never does.
   `show (1e23 :: Double)`: GHC `9.999999999999999e22`, tidepool `1.0e23`
   (auditor certain of this one; others need the oracle). Port Burger–Dybvig
   with boundaries excluded; add `1e23`, `9007199254740993`, `5.0e-324`,
   `1.7976931348623157e308`, `0.1`, `1e7`, `9999999.999999998` to a
   `FormattingExecutionContract` binding with a native GHC oracle.
2. Low, deliberate — `double2Int#` (`fallible.rs:182`) raises `Overflow` on
   NaN, ±Infinity, ≥ 2^63; GHC x86-64 returns `minBound`. Document the policy
   or match; oracle test over `truncate`/`round` on NaN and 1e19.
3. Low, Core-only — `decodeFloat_Int#` (`tidepool-bignum/src/lib.rs:85-90`,
   `host_fns/primops.rs:572`) decodes Float infinity as `(±1, 0)` and NaN as
   `(0, 0)`; GHC gives `(8388608, 105)` and raw-bit NaN. Prepared rejects the
   primop (not in its table).
Matching GHC: word/int carry and 2-word primops, `decodeDouble_Int64#`,
negative precedence handling, formatting authority, MD5 context layout,
native-bignum `Integer`/`Natural` (Haskell over word primops), `encode_double`.
Oracle gaps: `integerToDouble` rounding, `Natural` subtraction underflow.

## Extractor memo and cache invalidation
Rust cache keys are sound (endpoint identity hashes frontend, worker and GHC
libdir plus daemon epoch; undecodable cached artifacts recompile,
`artifacts.rs:485`); the invocation lane refuses to cache unknown flags;
`preparedSiblingsRef` is per compile.
1. Low — a memo hit merges siblings with `Map.union cached known`
   (`GhcPipeline.hs:772`), so a hit module's stale sibling map overwrites a
   freshly recompiled sibling Id (e.g. `Tidepool.Actors.Unfold` edited under a
   running daemon); a later importer is rewritten against the old Id (Core Lint
   failure or wrong call type). Use `Map.union known cached` or re-merge only
   the module's own siblings. Test: three resident requests with a sibling type
   change and a non-importing module between.
2. Medium-low — memo validity uses `ms_hs_hash` (source bytes only), missing
   CPP `#include` files and TH/quasiquoter dependent files; the Rust layer
   recompiles but the daemon memo returns stale Core. Skip the memo for CPP or
   modules with dependent files. Affects user include dirs (stdlib has no CPP).

## ExecutionProjection partiality and determinism
No wrong code or nondeterminism from ordinary programs; no `!!`, `head`,
`fromJust`, `error` or reachable incomplete patterns. Ordering is
deterministic (`nonDetEltsUniqSet` only feeds a `Set`, maps keyed by
`SymbolIdentity`, `dVarSetElems` captures, emission-ordered local spellings).
1. Conditional wrong code — `projectReference` checks `wiredInErrorKind`
   before the program's own tops and adds an implicit top under
   `idSymbol "value"` without a collision check (`:782-783`, `:873-892`);
   projecting a module that defines `patError`/`absentErr` yields duplicate
   symbols. `preparedTargetReferences` (`:249-258`) also does not exclude
   wired-in or deferred ids. Add the `topValues` check used at `:914`; filter
   `wiredInErrorKind` ids from references.
2. Smell — bottoming primop list (`:674`) lacks `RaiseOverflowOp` and
   `RaiseIOOp`; polymorphic results of those are rejected instead of
   `NoSuccess`.
3. Smell — `bindValue` does not remove the binder from `joins` and `bindJoin`
   not from `values` (`:954-970`); a reused unique could turn a value into a
   `Jump`.
4. Smell — exported `projectPrepared` (`:161`) skips `pmSiteRejections`
   (only tests use it); share the check or unexport.
5. Performance — signature, constructor and operation interning use linear
   search plus `<>` (`:1036-1087`, `:1205`): quadratic on large programs; use a
   `Map` index and `Seq`.

