# tidepool-codegen — Cranelift JIT and effect machine

## Charter

This crate compiles `CoreExpr` to native code, implements the JIT effect
machine, walks JIT frames for GC, and bridges live heap values to the reference
`Value` representation. Core IR belongs to `tidepool-repr`; high-level compile
and session orchestration belongs to `tidepool-runtime`.

## Suspension paths

Every suspended continuation lives in the parked-continuation registry as a
registered GC root. Callers that permit only one active continuation enforce
that policy above the JIT while still carrying its `ContinuationId` explicitly.

Both engines park in the same `ResourceLedger`. A frame carries per-engine
evidence: `FrameEvidence::Core` holds the Core constructor snapshot;
`FrameEvidence::Prepared` holds the site's evidence owner, site id, runner
program and admitted resume entry. A prepared frame's cell is the
continuation handle's own root slot, moved from the persistent-root list to
the stowed-root list for the park; the rooting receipt
(`stowed_roots_count() == parked_count()`) holds for both machines.

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

A parked continuation remains registry-rooted while ordinary fragments run or
other continuations resume. There is no separate child-run mode in the JIT.

## Root accounting

Keep these classes observable separately:

1. parked/stowed continuation roots;
2. value-handle roots;
3. persistent binding roots grouped by runtime resource scope;
4. the persistent-root ledger traced by GC.

Scope retirement removes registrations and reports what it released. It does
not compact `OldSpace` or reclaim its slot cells. Do not describe deregistration
as immediate memory reclamation.

The prepared machine reports these classes separately as `ResidencyCounts`:
programs, block words, persistent roots, handles, code exports, parked frames,
stack-map links, static regions, and descriptor/callable/enter rows. One leak
must not be able to hide behind another counter staying flat. Code exports
(`retain_export_top`) are a machine-lifetime class of their own: they belong to
no realm, grow only as installs reach new package tops, and are counted apart
from a turn's handles so neither can mask the other. Program retirement
happens only under the `Quiescent` token, minted by `PreparedMachine::quiesce`
and consumed by `collect_major`: a non-moving liveness mark over every root
class finds each program still reachable, and only an unreachable program's
block, descriptor rows, call/enter rows, and code are retired.

Every path that can allocate or force must install the full registry set used
by collection: stack roots, run roots, persistent roots, stowed roots,
remembered slots, and VM tail-call slots.

## Value handles and scope closure

`ValueHandle` is an opaque machine-side reference to a persistent root. It
allows a closure or other opaque heap value to move between parked
continuations without serialization. Observation may bridge a closure as the
documented sentinel; delivery uses the heap pointer itself.

Observation has two budget contracts, chosen by the caller through
`heap_bridge::BudgetPolicy`: `Complete` fails the whole decode when the budget
runs out, and `Bounded` stops there and leaves `OVERSIZE_SENTINEL` at the cut.
A bounded result is a selection, never the whole value; the value itself stays
reachable through its retained handle.

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

## JIT allocation

`CodegenPipeline` uses Cranelift's on-demand system memory provider. At the
shared function-definition entry it clears colocation hints for function and
data references; libcalls also use long-range addressing. Symbol visibility
does not promise physical proximity. New compilation rounds leave existing
code and data addresses stable without requiring a fixed contiguous arena.

`OwnedJitModule` releases all code/data allocations on destruction, including
partially compiled modules. Raw pointers may be used only while that owner is
alive. A machine runs synchronously under its owner and tears down its rooted
continuations and heap before module destruction. Actor/scope retirement leaves
the shared machine and its code alive for surviving captures and recipients.

Code still accumulates within a live machine. Removing binding roots does not
reclaim code. Live code reclamation requires tracking closures and continuations;
machine replacement is not transparent recovery for pending typed requests.

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
