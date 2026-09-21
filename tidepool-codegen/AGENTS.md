# Prepared-STG codegen and runtime

This crate validates prepared execution programs, lowers them to Cranelift, and owns the heap, collector integration, root ledger, cancellation, and continuation machine. High-level compilation, sessions, and actor policy belong above it.

- Every parked continuation has an explicit `ContinuationId` and registered roots.
- Initial and resumed completion share one settlement implementation.
- Keep continuation, value-handle, binding, and code-export roots separately observable.
- Every allocating path installs the complete root set used by collection.
- Heap mutations record generational edges at the store.
- Unknown handles, continuations, and runtime shapes are typed failures.
- Reject invalid resume answers before consuming the continuation.
