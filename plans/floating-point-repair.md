# Floating-point repair integration contract

Starting source: ee2e9a29. Root owns final integration and cross-lane acceptance.
FLOATING_POINT_BUG_REPORT.md supplies acceptance criteria, not a proven diagnosis.
Live root reproduction confirms both Float/Double classification returns True
for 1.0 and ordinary Prelude Show returns -NaN, while Render returns 1.0.

## Boundaries

* Numeric lead owns Translate.hs foreign-call recognition, primitive vocabulary
  in tidepool-repr, evaluator numeric arms, and JIT numeric primitive arms.
  Proposed six operations: FfiIsFloatNaN, FfiIsFloatInfinite,
  FfiIsFloatNegativeZero, FfiIsDoubleNaN, FfiIsDoubleInfinite,
  FfiIsDoubleNegativeZero. Confirm active GHC symbols and ABI first. Each takes
  the matching float literal class and returns an Int# predicate (0 or 1).
  Keep serialization's named vocabulary; inspect version/cache consumers.
  Replace pretty-printed substring matching with structured exact foreign target
  recognition if the active GHC API supports it. Do not add another registry.
* Strictness lead owns case/forcing/poison/heap boundary correctness, including
  primop.rs unboxing helpers but NOT numeric operation match arms. Trace a
  minimal unsupported call all the way to wrong success before repair. Root
  observed emit_lit_dispatch loads LIT_VALUE_OFFSET without a final literal
  shape check, and heap_force only enters thunks, not lazy poison closures.
  This is a candidate mechanism, not yet proof. Preserve dead-code laziness.
  Fix the owning strict-demand boundary, not classifier-specific checks.
* Validation lead owns new test fixtures, tests, test-suite registrations and
  test-only wiring. NumericContract.hs is a shared pure native-GHC/extractor
  input interface; consume/extend it rather than inventing another formatter.
  Existing differential runner remains the owner for JIT/eval comparison.
  Use native GHC additionally: shared lowering can fool both interpreters.
  No production formatter changes are authorized as a substitute for repair.

## Scaffold and remaining holes

NumericContract.hs defines typed classification, standard Show observations, and
finite/special edge construction for both widths. It is a deliberately partial
instrument: no runner yet, no claimed passing Tidepool behavior, no generated
bit-pattern corpus yet, and no unsupported-operation fixtures yet. Validation
lead owns closing those holes and adding actual derived numeric-record Show.
No production enum stub or fake success is installed. Numeric lead commits the
coherent vocabulary and representative consumers before its own worker split.

All leads use resident unfold/fold, exact committed seeds, independent owned
worker scopes, focused checks, and a fresh-context review/repair wave. They may
recurse as useful; do not run broad batteries in worktree waves. Shared file
ownership is per section as above; coordinate consequential interface changes
through root. Return exact candidate, checks, evidence limits and remaining
choices with retained worker handles. Root merges reviewed lanes, runs relevant
integration tests and fixtures-check, then probes a rebuilt resident runtime
(or explicitly reports that the running host has not incorporated repairs).
