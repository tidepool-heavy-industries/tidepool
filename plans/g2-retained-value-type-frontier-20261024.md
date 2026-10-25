# G2 retained-value TYPE frontier

Status: source- and retained-byte diagnosis; repair not implemented. This is
separate from the active offer-issuance/package-contract wave. Review the
boundary before adding another projection to that wave.

Code under examination: `bff9e561755246ace2fe8ff139441844c4067a04`.
The current integration includes only a logging change after that code:
`6dfcedc36905c6faf212754667d747d7b404ec23`.

## Confirmed failure

The captured-source actor test
`resident_captured_source_retains_the_parked_job_for_a_later_cell` publishes
`job1`, then submits `_ <- Cmd.await job1\npure (42 :: Int)`. The compiler
rejects the later cell in `readWorldTypeInputs`:
`demanded type lacks its exact Context association`.

The interrupted outer runner did not preserve its Rust panic/count receipt.
The owned-service report records child exit 101 and reaped compiler processes.
Do not infer OOM from the outer `Killed` text or count this as a completed
runner acceptance result. Recovery evidence is below.

The retained packets establish the missing relationship:

- Initial `.tmpKi86vg/world.cbor` contains no `Tidepool.Command.Types`.
- `.tmpv2hD3G/successor.cbor` publishes `job1` as
  `main:Tidepool.Session.Val.G2`, whose exact requirements include
  `(main,Tidepool.Command.Types)`.
- All three retained successors have an empty primary-association change list.
  The last successor carries only G1/G2's own interface/package inputs.

The raw bytes, file hashes and read-only decoder are in
`/srv/swarm/evidence/inanna/g2-offer-issuance-20261024/job-type/decoded-evidence.json`
and `inspect_cbor.py`. The failing transaction is
`/srv/swarm/evidence/inanna/g2-source-owner-20261024/joined/actor-bff9e56175/e4e36d94370f6455d066b12cb3d583164630ccb844bad0310c3c6fb3aadf4c6d/artifacts/compiler-transactions/.tmptAUgRA`.

## Ownership trace

The Rust checked value retains its exact TYPE support. It is not lost custody:

1. `ExactHostBindingPrototype` retains `SelectedTypeInterfaces`; both cold
   checked-host compilation and fresh thin-interface reuse retain complete
   support in `CheckedValueArtifact`.
2. `CompilerWorld::with_visible_host_bindings_resuming` publishes the checked
   row through `project_bindings_with_primary(..., None)`.
3. `WorldColdRows::row` transports that row's own interface and package bytes,
   dependency owner names, and row/receipt identity. It does not transport the
   dependencies' exact Context associations. Cold restoration uses the same
   single-row representation.
4. Haskell `readWorldSelectedBindingInputs` immediately asks
   `readWorldTypeInputs` to resolve those names through held primary facts.
5. Rust `WorldProgramCursor::stage_inputs` already borrows the demanded checked
   values' TYPE closure, but that happens after Haskell has produced the stage
   receipt. It cannot supply this earlier check.

Ordinary checked values also deliberately keep their TYPE closure separate
from the World's permanent declaration/source namespace. Permanently merging
all retained TYPEs into primary would erase that distinction and could make
unused historical versions conflict. A module-name fallback or searching broad
custody would likewise invent a selection.

## Proposed boundary for peer review

Carry an immutable, row-coupled TYPE projection from the checked value's
existing support to Haskell before the selected row is checked. Prefer the
existing Context/materialization owners and exact references; do not introduce
a global inventory or copy full contexts into every consumer.

The proposed contract is:

- The projection is issued from `SelectedTypeInterfaces::from_checked_value`,
  correlated with the exact checked row, producer, and semantic identity.
- Cold capture, successor publication, promotion, and recovery preserve it.
- Haskell borrows only selected rows' projections for the stage, preserving the
  distinction between TYPE, SOURCE, and executable demand.
- The Rust receipt-side borrow uses the same issued selection. Missing,
  substituted, or conflicting evidence refuses before execution.
- Unselected historical alternatives remain legal.

Open design questions for Argon:

1. Should a bound row retain a Context root plus immutable arena references,
   or a smaller issued TYPE-closure product using the existing materializer?
2. Where should exact row/projection correlation be checked so omission is a
   construction error rather than a later lookup failure?
3. Can cold and successor encoders share one complete row product, eliminating
   the present opportunity to serialize the leaf without its required support?

## Discriminating checks

Keep the real captured Job history and backend completion-count oracle. Add an
ordinary checked nominal value from a module absent at bootstrap, then consume
it in a later cell. Compare cold and reused host-carrier issuance. Assert that
permanent primary associations do not change merely because a value is visible.

Exercise missing/substituted closure refusal, selected conflicting versions,
unselected alternate versions, and cold/successor/recovery preservation. The
existing Job fixtures explicitly import `Tidepool.Command.Types` and do not
prove this missing-bootstrap-association premise.

Detailed independent Rust trace and test-gap analysis:
`/srv/swarm/evidence/inanna/g2-offer-issuance-20261024/job-type/review.md`.

Separate cleanup frontier:
`/srv/swarm/evidence/inanna/g2-offer-issuance-20261024/runner-recovery/recovery-report.json`.
The package provider released 75 of 76 roots, retaining its Python guard, then
exited 120 before Rust wrote the native finalization record. The supported
finisher refuses the absent record. No manual root/record surgery was done;
this requires its own authenticated retry-state repair.
