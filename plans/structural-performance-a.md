# Structural performance: measurements and open questions

Status: matched measurements are complete; open questions below have no
implementation commitment.

The matched cost tests in `bridge/facade/src/actor_host/cell_compile_cost_tests.rs`
are the current performance evidence. They report compiler request counts,
wall time, and machine snapshots against a resident compiler daemon. Timing
varies between repeats; do not infer savings from modeled estimates or sum
overlapping phase and module measurements.

## Accepted contracts

- Immutable compiler support belongs to the toolchain epoch and the existing
  artifact/export owners. Session Lib/Val and effect-row shims remain mutable
  home modules. Any package interface must satisfy ordinary GHC consumers and
  intrinsic authority.
- Display observes an expression once. Publish its observation before a
  presentation failure; pass runtime budgets and keys as values rather than
  specializing generated source.
- Host mounts use typed construction and existing machine-owned handles.
  Validate type and machine-session identity before installation.
- Temporary root chunks keep stable addresses. Reusable DAG handles remain
  rooted; tree consumption happens only after parent publication. No borrowed
  heap view survives collection or forcing.
- Static-region indices select candidates only. Exact object/tag validation
  and transactional installation and retirement remain authoritative.
- Embedded prepared fixtures name their actual producers and schema contracts.
  Regenerate through those producers; never patch version headers.

The runtime and toolchain ownership guides carry these contracts for routine
implementation work.

## Final matched measurements

The final integrated frozen-workload run kept request counts at two for actor
activation, one for unchanged reload, two for edited reload, three for a
recurring one-statement cell, eight for a recurring six-statement cell, and one
for lookup. First measured one-statement cell wall time was 4.607 s. Recurring
one-statement cells took 860 and 1,438 ms; six-statement cells took 2,473 and
3,540 ms; lookup took 33 ms. The repeated cell timings were mixed, and no
latency speedup is claimed.

The final 13-program machine snapshot contained 12,113 native functions,
4,476,101 native-code bytes, and 1,545 persistent roots. Live old bytes were
10,536. These figures are snapshots for this workload, not general memory or
throughput claims. Raw diagnostics live under ignored `target/` measurement
directories.

The matched compiler/runtime/GC acceptance passed, including all seven
registered schema-14 embedded artifacts. `just quick` passed 726 tests with
one ignored, and supported Cargo all-target compilation passed. The hours-long
`just verify` was not run. Earlier intermediate failures and historical parcel
completion notes are omitted; use Git history for that record.

## Open questions and cost axis

| Question | Runtime and native cost | Build, memory, and dependency cost | Correctness and maintenance gate |
| --- | --- | --- | --- |
| Can display execution be reused across cells or installs? | Display currently compiles a fresh display program per displayed cell; the matched cost test does not isolate the cost of reuse. | Could reduce cumulative generated code and retained programs; may add callable/alias plumbing and mount work. No separate artifact cache or owner is justified yet. | Preserve fresh keys and budgets, single execution, observation-before-display-failure, and partial commits. |
| Can a checked cell compile to one per-cell execution bundle? | The current path makes separate compiler requests for statements; no matched bundle measurement exists. Measure request count and wall/phase time against the one- and six-statement baselines. | Bundling may reduce request overhead while changing generated-code size, retained state, and failure scope. | Preserve statement order, intermediate bindings, per-statement commit/rejection behavior, and final display semantics. |
| Can the six-statement request be batched? | It makes 8 requests and measured 2.473–3.540 s, versus 3 requests and 0.860–1.438 s for one statement. This is the clearest request-count opportunity. | Likely reduces repeated compiler startup/request work; may increase per-request compilation size, code volume, and failure blast radius. | Keep source order, prefix commits, effect receipts, cancellation boundaries, and precise rejection location. Compare matched wall/phase and machine snapshots. |
| Can immutable actor-spec compilation be reused across actors under exact identity? | Repeated preparation may still be visible, but no controlled current attribution establishes its share. | Could save compilation and generated code; exact source/import/effect/toolchain identity, dependency fanout, and cache retention add complexity. Every actor must still receive fresh machine and live-handle state. | Measure repeated preparation first. Never share mutable machine state; require invalidation and isolation tests. |
| Which of the 12,113 retained native functions are still demanded? | The final measured machine held 4,476,101 native-code bytes. Counts alone do not show whether traversal, dispatch, or execution is costly. | Demand evidence may identify reclaimable code, but tracking can add compile/install work and retained metadata. | Attribute live versus cumulative functions by owner and workload before pruning broad exports, adapters, display code, or retired generations. Preserve callable reachability and retirement safety. |
| Is `Tidepool.Patch`'s parser/Myers implementation a material production cost? | Generated `planUpdate` calls `Patch.genPatch` and `Patch.renderPatch`; the Haskell implementation converts `Text` to linked `String`, uses a linked-list Myers frontier with repeated indexing, and scans hunk windows. No representative latency or allocation profile is recorded. | A Rust boundary could reduce allocation and indexing cost but adds protocol, generated code, and maintenance surface. | Measure real file sizes, edit distances, allocations, and native bytes first. Preserve the Haskell ADT, context matching, ambiguity reporting, and round-trip properties. |
