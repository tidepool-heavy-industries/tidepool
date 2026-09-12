# Notebook lane

Own GHC cell request through actual actor workbench execution, Generic, display,
and cell corpus. Resolve current source paths: the proposed
haskell/src/Tidepool/Inspection.hs was absent in initial inspection.

First scaffold: worker/runtime request and result types, source spans and grouped
declaration execution, shared fixtures. GHC owns syntax; Rust never reparses it.
Parent retains synthesis/execution agreement, identity and actor consumer wiring.
Fork lexer/classification, execution/receipts/recovery, and Generic/display from
usable shared interfaces, scheduling in local waves rather than simultaneously
launching every obligation. Children can recursively split substantive work.

Stage 1 requires whole-cell inferred types pinned into each staged compile.
Consult fresh Astra on post-zonk statement binder harvesting and faithful type
transport: tcg_type_env/top-level capture alone is insufficient. Test numeric
downstream inference first, then a child Response whose result is fixed downstream
to a type declared in the same cell. Supply complete, constructor-exhaustive
fixtures; peer's abbreviated example is not runnable evidence. Cover constraints,
polymorphism, inaccessible tycons/skolems and nominal identity separately.
If transport fails, staged checking is sound but weakens product inference:
escalate rather than quietly keeping annotation tax.

Verify and reuse declaration Lib.G generation/reexports, Val.G iface injection,
incarnation sealing, replay and prefix machinery; peer supplied these as source
leads, not checked conclusions. Checking installs nothing. Do not reinterpret
parked continuations as permission to run the tail after respond.

Display parent/child establishes last type, expansion semantics, per-cell budget
and command paging integration before forking renderer/runtime storage work.
Fresh Astra slot for consequential continuation typing. Record Stage 2 heap
custody constraints only; no speculative second implementation.

First readback includes failing-today numeric and same-cell nominal-type inference
fixtures, concrete cell/receipt/next-cell walkthrough and recovery case, actual
file ownership, child frontier and parent integration checks.
