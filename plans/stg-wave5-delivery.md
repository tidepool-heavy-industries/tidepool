# Wave 5 delivery

Finish recovered-body and lazy execution delivery before Wave 6 effect-interpreter
integration. Delivery takes precedence over delegation experiments and unused
primitive-family completeness. The prior verified checkpoint is `800ab0d06`;
its 173 matching fixture keys are evidence, not completion.

## Current evidence

`59945d6e6` closes the pending mutable-array-copy batch: 163 prepared-program
tests and 42 machine-state tests passed through `scripts/dev-shell.sh cargo test
-p tidepool-codegen --lib <filter> -- --nocapture` with filters
`prepared_program::` and `machine_state::`. Logs remain in
`target/w5-delivery-{prepared,machine}.log`.

Schema migration `6c86ca470` has 242 repr library tests and 12 codec tests
passing; Haskell execution-schema-encode passes. Two old producer fixtures
remain schema 7 until the final coordinated regeneration. This is not a full
fixture gate. Workspace formatting is isolated in `93e1b25f5`.

The three historical STG JSON ledgers are removed from the active tree by
request; `800ab0d06` preserves them in Git. Corpus evidence is reproducible via
`env -u TIDEPOOL_EXTRACT -u TIDEPOOL_EXTRACT_WORKER just fixtures-check`.
Its generated manifests/results remain build artifacts, not new committed logs.

## Order and owners

1. Repair pending mutable-copy test fixtures; run prepared-program and
   machine-state tests and commit the isolated batch.
2. Walk recovered STG for operation identities/signatures, literal labels and
   recovery residuals without changing production projection. Missing bodies
   remain explicit boundaries of this inventory. Catalog from this evidence,
   not from a guessed spelling list.
3. One schema migration adds deferred capabilities and wired-in errors.
   Capability operations retain Returns; wired-in errors use NoSuccess.
   Haskell encoder, Rust decoder, validation and the existing cross-language
   contract migrate together. Unknown identities remain rejected.
4. Implement live-path MutVar new/read/write through the external-storage and
   barrier owner with a distinct descriptor. noDuplicate is a no-op only under
   invocation-private, serialized, nonconcurrent execution. Cancellation/retry
   restarts evaluation; it is not GHC stack resumption. Atomic variants defer.
5. Lower exact catalogued missing functions as ordinary synthesized callable
   tops so bare references and PAPs work. Recognize wired-ins by GHC keys.
   Catalogued labels, if still present after the defining capability boundary,
   fail through an Address-returning operation, never a fabricated pointer.
6. Run the corpus and close newly exposed live-path gaps. Format the workspace
   in a separate commit, run the broad gate, remove the three STG JSON ledgers,
   and publish a compact handoff with commands and named limitations.

## Failure contracts

- Deferred capability execution records UnsupportedCapability as the first
  cause, publishes no result, and leaves a sound machine reusable. An unknown
  import is not automatically a deferred capability.
- patError and nonExhaustiveGuardsError are PatternMatchFailure; recConError,
  noMethodBindingError, recSelError and typeError retain distinct language kinds.
  Preserve GHC UTF-8 message formatting, including recSelError's prefix.
- absentError, absentConstraintError, absentSumFieldError, impossibleError and
  impossibleConstraintError are typed terminal integrity failures. Treating
  impossible workers as terminal is an intentional stricter policy than GHC's
  ErrorCall rendering: reaching them violates a compiler invariant.
- Read error strings only inside authenticated pinned storage with a bounded
  NUL scan. Invalid storage is integrity failure, never a partial message.
- Reusable failures restore thunk state/captures; terminal failures do not
  dereference a potentially damaged heap. Preserve the existing first cause.

## Acceptance

216 finite Suite fixture keys match with no comparison mismatch; the optimized
non-finite blackhole remains explicitly omitted. Do not shrink the denominator
or classify harness omissions as successes. Capability and wired-in retry,
first-cause, no-output and MutVar moving-GC tests pass. Add structural expectations
for Project.Work.candidate and an awaitSettled dependency-only probe; execution
of its continuation remains Wave 6, not a hidden requirement here.

Run focused contracts, workspace test-target compilation, canonical fixture
check and the relevant broad changed gate. Record actual results separately.
Real stack snapshots, IPE/libdw, atomic MutVars and unused primitive gaps remain
explicitly deferred; no Core fallback is introduced.

## Execution discipline

Lead owns semantic seeds and cross-language decisions. Delegate bounded
implementation, inventory and checks where they reduce total work. No worker
headcount target. Shared-tree builds have one acknowledged lease; production
schema edits wait until the pending test batch completes. Use fresh review for
failure semantics and pointer ownership, not another general audit wave.
