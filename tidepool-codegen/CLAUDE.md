# tidepool-codegen — Cranelift JIT and effect machine

## Charter

This crate compiles `CoreExpr` to native code, implements the JIT effect
machine, walks JIT frames for GC, and bridges live heap values to the reference
`Value` representation. Core IR belongs to `tidepool-repr`; high-level compile
and session orchestration belongs to `tidepool-runtime`.

## Suspension paths

There are two suspension mechanisms:

- a single slot used by one-shot eval and the REPL;
- a parked-continuation registry used by the resident harness.

They must never coexist on one machine. The single slot is protected by the
rule that the machine does not run while suspended; parked continuations are
registered GC roots and may be resumed in any order. Mixing them would let a
collection free the unregistered slot continuation.

The public parked-path contract is
`docs/continuation-parking-contract.md`. Keep frame layout and helper plumbing
private.

## Materialization

Initial completion and resumed completion share one materialization
implementation. The four policies are:

- return a bridged value;
- bind a persistent root, optionally forcing first;
- project and root product fields;
- root field zero and bridge field one for REPL rendering.

Do not create a resume-only epilogue. Ordering in render materialization is
alias-sensitive: bridge the rendered field before tenuring the bound field.

## Nested runs

A suspended parent may run sequential child fragments through
`run_child_fragment*`; its continuation remains registry-rooted throughout.
Registry-native consumers can run child work as an ordinary fragment while
other frames remain parked.

## Root accounting

Keep these classes observable separately:

1. parked/stowed continuation roots;
2. value-handle roots;
3. persistent binding roots grouped by runtime resource scope;
4. the persistent-root ledger traced by GC.

Scope retirement removes registrations and reports what it released. It does
not compact `OldSpace` or reclaim its slot cells. Do not describe deregistration
as immediate memory reclamation.

Every path that can allocate or force must install the full registry set used
by collection: stack roots, run roots, persistent roots, stowed roots,
remembered slots, and VM tail-call slots.

## Value handles and scope closure

`ValueHandle` is an opaque machine-side reference to a persistent root. It
allows a closure or other opaque heap value to move between parked
continuations without serialization. Observation may bridge a closure as the
documented sentinel; delivery uses the heap pointer itself.

`close_realm` releases a runtime resource scope's frames, handles, and
cancellation state while leaving siblings untouched. Unknown handles and
continuation IDs are typed errors.

## Failure behavior

- Unexpected runtime shapes produce a poisoned result with a useful breadcrumb,
  not SIGILL or an invented fallback value.
- Cancellation checks belong at tail-call and other established safepoints.
- Unresolved external IDs should report their qualified name when metadata
  provides it.
- A resume answer containing bottom is rejected before consuming its
  continuation.

## Diagnostics

Diagnostics are opt-in and must remain off in normal execution. Search the
owning module for the current variables before adding a new knob. Prefer an
existing trace for GC, case traps, unresolved externals, or compiled-function
inspection over a parallel logging path.

Test-only forcing and GC hooks must be `#[doc(hidden)]`, deterministic, and
named for the exact state they create. They are not production recovery APIs.

## Verification

Use focused tests for:

- JIT/oracle differential behavior;
- suspension and resume materialization;
- parked continuation rooting and arbitrary resume order;
- handled-prefix refusal;
- GC during tenure, nested runs, and handle observation;
- scope closure and root-count receipts;
- case traps, cancellation, and unresolved externals.

```bash
cargo nextest run -p tidepool-codegen
```
