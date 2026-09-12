# M2 final-source session disposition join (r7)

Status: reviewed repairs integrated and checked for the Applications A7 seam.
This is M2/session evidence, not M6 invalidation or complete resident
introspection acceptance.

## Source and reviewed repairs

The join starts from released engine assignment
`c493c1f403b9309e2306edbce85956ab4c36dc06`. A selected fresh independent
review found that major collection did not traverse live old-owner edges into
the nursery. The first review admission failed before actor startup; its single
preserved retry executed and reported the defect.

Integrated repair commits:

- `ff988dc94d3df761b146f88b89377bd82cb5ba30`: major fixed-point traversal now
  exposes pointer slots from reachable old objects, allowing nursery tracing to
  discover old-to-young edges without treating remembered edges as strong roots.
- `942284b5237f613a782c26b08f912a3a01150d5f`: `PersistentSession` exposes
  `machine_disposition()` and its real language/integrity/cancellation consumers
  use that boundary. Retirement comments now match quiescent major reclamation;
  the stale `CLAUDE.md` reference is gone.
- `be5f8ada247d759cfe2788587f9d601ff6c05511`: nursery reachability normalizes
  exact forwarded stubs in root/object slots before tracing, while retaining
  rejection of non-object interior pointers. This closes the `BadPointer`
  exposed by the real resident introspection consumer.
- `ceec332519bd248cd87b425c021d0a72c25b4196` is included only as the concrete
  dependent resident `Eff` consumer. Its remaining public-module lookup failure
  belongs to introspection, not M2.

The owning changed functions are:

- `JitEffectMachine::collect_major_quiescent` and
  `OldSpaceCompaction::source_pointer_slots`;
- `trace_heap_region`/`enqueue_region_slot` forwarded-reference normalization;
- `PersistentSession::machine_disposition`, `ensure_machine_reusable`, and the
  language-error, integrity-failure and cancellation/reset consumers;
- retirement contracts on `PersistentSession::retire_scope`,
  `JitEffectMachine::retire_scope_root`, and `abandon_uncommitted_root`.

No `recovery.rs` or recovery-facing `session/mod.rs` entry point was edited.
`DeclarationRecoveryReport` remains the distinct replayed/lost source boundary;
it cannot recreate resident values, heap identity, handles or grants.

## Resulting-source evidence

Focused checks after integration:

- `just test-lib tidepool-codegen` with exact forwarded-child, live-old-owner,
  wrapper external reachability, remembered-vs-strong, and missing-root cases:
  5 executed, 5 passed, 136 skipped.
- `just test-lib tidepool-runtime` with exact language-reuse,
  integrity-unavailable and cancellation/reset cases: 3 executed, 3 passed,
  150 skipped.
- `just test-target tidepool-runtime session` selecting
  `session_decl_recovery::successor_replays_only_root_source_independent_of_resident_values`:
  1 executed, 1 passed, 95 skipped.
- `just test-target tidepool-codegen resident` selecting retirement major
  collection, live-park drop cleanup, realm cancellation/sibling isolation and
  retained external-environment root survival: 4 executed, 4 passed, 58 skipped.
- exact resident introspection consumer: the pre-repair run failed before its
  first result with `BadPointer`; after the forwarded-reference repair it makes
  both resident calls and obtains identical structured results, then fails only
  at the later expected public-module name (`public-error` versus
  `ActorDefinition`). Thus M2 fixed the integrity failure; introspection still
  owes its public-module lookup repair and final passing rerun.
- direct `rustfmt --edition 2021 --check` on every changed Rust file and
  `git diff --check`: passed. Package-wide `cargo fmt -p tidepool-runtime
  --check` remains blocked by pre-existing ordering in
  `tidepool-runtime/tests/suites/session.rs`; that file is unchanged by M2.

One initial retirement command incorrectly targeted the `codegen` suite and
selected zero tests (exit 4). The corrected `resident` selection above is the
evidence; the zero-test run is not.

## A7 handoff and remaining gate

Applications A7 may start from this checked session boundary. Language failures
leave the machine reusable; integrity failures make subsequent entry return
`JitError::MachineUnavailable`; cancellation remains reusable only after the
consumer explicitly resets its cancel flag. `machine_disposition()` reports
that two-state reentry decision; source replay uses the separate checked
replayed/lost report.

M6 still owns artifact/schema/profile invalidation, production writer/reader and
resident cutover, old-path deletion and no-fallback proof. A7 must be rechecked
on the final M6/combined source, with concrete `session/mod.rs` edits serialized
by the coordinator. Native acceptance here is x86_64; aarch64 remains
unavailable and unverified.
