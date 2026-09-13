# Wave 4 follow-up: binder identity and honest corpus coverage

Baseline: `05e27a073`. This closes projection and measurement defects before
Wave 5; it does not implement laziness, imports, effects, or resident cutover.

Completed baseline verification: workspace test targets compile; repr 2/2,
runtime 6/6, corpus comparator 7/7, runner 7/7 pass. Engine contracts are 26/27:
the remaining self-capture fixture uses Atom encoding where ValueRef is required.
Suite comparison is now 56 passed, zero mismatches, one missing expectation.
Project.Work.candidate passes through execution with no expectation; the actor
cohort lacks its generated production Effects.Core environment. Fixture freshness
is stale. Logs are in `target/wave4-checkpoint-checks/`; these results precede
the follow-up edits, not evidence that the new manifest/reachability works.

## Shared decisions

Reachability uses GHC binder Unique throughout the existing inventory walk.
Only the complete-module VarEnv converts that identity to a stable wire symbol.
An absent projected home top is a typed projection failure, never an import.
Unknown internal names likewise cannot become imports. Genuine external package
imports remain valid; diagnostics may describe internal names, but fake
`<interactive>:<local>` identities must not be emitted as import declarations.

Corpus manifest version 2 enumerates actual prepared top identities in the
selected module before filtering. Each row has its full canonical identity key
and an explicit optional historical expectation key. Only exact external tops
may match historical expectation names. No suffix stripping or guessed aliases.
The old artifact-name list is retained separately as a mapping ledger: exact
external match or explicitly unmapped. Missing old locals are unmeasured, not
failed projections, and never silently dropped from the legacy denominator.
Report both the STG top count and the legacy mapping count; they are not the
same population. This cannot promise that 92 old locals become executable tops.

## Parcels and ownership

1. A, Luna High: `ExecutionIR.hs`, `ExecutionProjection.hs`, and projection tests.
   Finish the lead's binder-identity seed; regress distinct internal same-name
   references, missing home tops, and real imports. Preserve emission order and
   deterministic wire identities. No projection schema bump.
2. B, Luna High: Haskell corpus probe and `scripts/prepared-corpus.sh` only.
   Implement manifest v2 producer and exact mapping from the shared Rust types.
   Default requested-target mode remains for priority cohorts; `--all-tops`
   enumerates Suite and treats its supplied target file as the legacy ledger.
   Production actor environments must not be replaced with test stubs.
3. C, Luna High: Rust corpus types/runner/tests and the one malformed codegen
   rejection fixture. Consume v2, preserve mapping in reports, select oracle by
   explicit key only; fix capture wire encoding without relaxing validation.

A, B, C run concurrently with disjoint ownership. One build slot, A then C then
B. Lead reviews semantic diffs; workers run focused checks. Two unsuccessful
worker attempts escalate with evidence. Fixtures regenerate only after A and B
freeze; final corpus, freshness, and workspace compile run once at the fold.
No inference that all corpus reds are runtime defects or that all are harness
defects. Classify using the actual failing stage and source evidence.

## Acceptance and trial record

Distinct same-occurrence tops survive target closure; no phantom local import.
Manifest tests distinguish internal tops from external oracle keys, preserve
unmapped legacy names, reject ambiguous mappings, and retain every actual top.
The malformed nested rejection fixture reaches its intended compiler assertion.
Record the final per-stage curve and all residual names/reasons, then push for
review. Update inventory/friction notes from observed outcomes, not promises.

Initial lead rounds: one ownership/dependency plan; one shared seed/edit round;
three worker assignments. Subsequent reviews/corrections are recorded as they
happen. Token accounting is unavailable. Outcome assessment remains required.

Harness thread retention prevented creating a third new worker and had evicted
the previous Rust worker. Parcel C therefore reuses `wave4_projection_read`
(Luna High) with a fresh explicit ownership assignment; it no longer owns the
Haskell probe. A and B are `identity_projection` and `identity_corpus_producer`.

Progress evidence:

- C: comparator 7/7, runner 12/12, prepared-program contracts 27/27 passed.
  The capture fixture now reaches its intended compiler rejection. One missing
  test-only import was corrected after compilation; no assertion was weakened.
- A: projection suite passed, but lead review found its new same-spelling test
  covered a local shadow of an external top, not two internal tops. Stronger
  collision coverage is required before accepting that parcel as complete.
- B: source-ready manifest review required all-top compilation failures to abort
  rather than emit a successful empty denominator; ambiguous legacy mappings
  must likewise fail explicitly. Pure mapping self-tests were then added.
- C review required the legacy name to equal the mapped external occurrence,
  rejecting an otherwise well-formed but forbidden suffix-stripping alias.
- A review removed duplicate identity allocation from its extra Unique-keyed
  lookup table: any auxiliary view must derive from the single identity owner.

Lead follow-up events so far: A identity-owner correction, A guard-order
clarification, A collision-test rejection; B empty-denominator/ambiguity
correction and self-test assignment; C legacy-alias correction; serialized test
grants and exact-result reviews. These events are counted contemporaneously;
earlier full-wave accounting remains incomplete.

The v2 corpus run is preserved as `plans/stg-wave4-corpus-identity.json`.
It enumerated 812 STG tops, of which 802 projected and all 802 validated.
Legacy mapping retained all 347 names: 255 exact mappings and 92 unmapped.
Of the 217 historical expectation keys, 207 have manifest oracle keys. Ten
rounding/printf names have exact legacy-to-STG identity mappings but no oracle
key on their rejected rows: STG projection rejects their foreign/prim calls.
Among the 207 mapped oracle cases, 56 admitted, executed, and matched; 151
failed closed-strict admission and never reached execution. Across all STG
tops, admission passed 191, execution passed 123, and comparison passed 56 with
67 missing expectations. The 68 execution failures are 21 entries requiring
managed host arguments, 46 Address observations, and one 100,000-node budget
exhaustion on `thunk_letrec_knot_xs`. These are separate current limitations,
not 68 evidence-backed wrong returned values.

The Project.Work priority target projects, validates, admits, compiles, and
runs; it has no comparison expectation. The actor stdlib target still fails
source compilation because the production generated Effects.Core module is
not present in this corpus setup. No tiny prepared-probe stub is substituted.

## Fold result on `a1e2f5299`

The strengthened `ho_myany` regression passed the Haskell
`execution-schema-projection` suite. It exercises a previously failing Suite
target with multiple distinct internal `sat` tops and asserts no fake internal
globals. The Rust prepared-program tests passed 27/27; the corpus runner passed
12/12 and comparator tests passed 7/7. The Haskell corpus mapping self-test
passed. Repr's cross-language schema contract passed 2/2.

`just fixtures-check` passed and replayed the 812-top corpus with the counts
above: 802 validated, 56 compared successfully, 67 without expectations.
`bash scripts/dev-shell.sh cargo build --workspace --tests` exited 0; this
compiled all test targets without executing them. `just fixtures-update`
changed only `haskell/test/suite_cbor/.source-fingerprint`, not semantic CBOR.
Its first invocation failed because preset `TIDEPOOL_EXTRACT_WORKER` was stale;
retrying with both `TIDEPOOL_EXTRACT` and `TIDEPOOL_EXTRACT_WORKER` unset rebuilt
the matched worker and succeeded. No Core interpreter tests were run.

Luna outcomes: B's mapping producer and C's typed consumer handled the bounded
contracts after one review correction each. A needed a lead correction to test
the actual two-internal-top failure, then passed against the real Suite case.
The initial scaffold's `Unique` lookup was invalid because a `VarEnv` is keyed
by `Var`; A derived a Unique-keyed view from the single identity owner. The
cross-language semantic seam and precise test oracle were the parts that
needed lead review. Exact worker token and full lead-round totals are not
available; none are inferred here.
