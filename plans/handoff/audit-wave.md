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

