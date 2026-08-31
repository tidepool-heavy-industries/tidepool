# Cranelift JIT and effect machine

This crate compiles `CoreExpr`, runs the JIT effect machine, traces JIT frames,
and bridges live heap values. High-level compilation, sessions, and actor policy
belong above it.

- Every parked continuation is an explicit `ContinuationId` registered as a GC
  root. Single-continuation policy belongs to callers, not a second JIT path.
- Initial and resumed completion use the same materialization implementation.
  Do not add resume-only epilogues or parallel suspension APIs.
- Keep parked-continuation, value-handle, persistent-binding, and persistent
  ledger roots separately observable. Deregistration does not imply immediate
  old-space reclamation.
- Every allocation/forcing path installs the complete root registry set used by
  collection. Partial registry installation is corruption, not an optimization.
- `ValueHandle` carries opaque live values between continuations without
  serialization. Unknown handles and continuations are typed errors.
- Unexpected runtime shapes produce a poisoned result with a useful breadcrumb;
  never emit SIGILL or fabricate a fallback value.
- Reject bottom-bearing resume answers before consuming the continuation.
- Diagnostic and forcing hooks are opt-in, deterministic, test-only where
  appropriate, and never promoted into production recovery APIs.
- Consult `docs/continuation-parking-contract.md` when touching suspension,
  rooting, resumption, or cleanup behavior.
