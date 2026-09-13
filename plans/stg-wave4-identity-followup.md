# Wave 4 follow-up: binder identity and honest corpus coverage

Baseline: `05e27a073`. This closes projection and measurement defects before
Wave 5; it does not implement laziness, imports, effects, or resident cutover.

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
