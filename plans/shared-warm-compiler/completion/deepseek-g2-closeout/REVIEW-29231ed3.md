# Peer review of the joined H/P/T/N checkpoint

Reviewed code: `29231ed3236bb4dd6d13b6e459315533cd171a3e`.
Incoming documentation tip: `5b3e15d7dc093fc7ea7f28f1592abfe65f150f36`.
Branch: `root/g2-final-20261021`. Comparison base: `90a5456584`.
The incoming checkout was clean. This review changes documentation only.
**Request changes; retain this consolidated branch; G2 is unqualified.**

Four independent Sol audits and root review traced the actual producers,
consumers and retained artifacts. No new compiler build or behavioral test was
run. Reports and copied producer artifacts are in
`/srv/swarm/evidence/inanna/g2-peer-review-29231ed3-20261022/`:
`private-progress.md`, `package-selection.md`, `support-text.md`,
`g2-completeness.md`, `retained-producer-observations.json` and
`job-producer-observations.json`. Line references below refer to the code candidate.

## Keep the useful work

- Current preparation uses the session's current World, separately from immutable
  birth authority. Accepted/applied host revisions and exact retirement have real owners.
- H composes prefix, exact stage, native availability and completion custody before
  closing support. The wrong-stage check is more precise than request identity.
- P removed broad inherited owner/ordinal selection. Ordinary and item HOME policy
  is shared. Retained producer output proves fresh transitive HOME demand.
- Production-source fixtures improve on fabricated authority. Declaration-only,
  host-only Text and simple retirement cases have useful retained passing evidence.

These are partial implementations of the right boundaries. Replacing the warm
compiler architecture is not warranted. Finishing ownership at the joins is.

## Confirmed defects and incomplete contracts

| Priority | Finding | Owning repair |
|---|---|---|
| P1 | Last program's immediate predecessor is compared with the entire private execution's birth World | Cumulative settled frontier, separate from local item cursor |
| P1 | Freeze can seal a parked program; snapshot issuance can capture accepted-but-unapplied revisions | Atomic lifecycle/readiness boundary, no mutation on premature freeze |
| P1 | Preparation claims active ownership too early; failed/cancelled ownership lacks a complete settlement path | Inert offers, affine first-entry claim, explicit terminal failure |
| P1 | Activation registers pending custody twice, then publishes bindings without a typed World intent | One mount owner and normal paired World publication |
| P1 | World typed callbacks receive an empty old-scope admission map | Feed already authenticated World type-origin evidence to host classification |
| P1 | Checked Text fails producer certification across stages; distinct predicates are collapsed | Typed refusal first, then repair the exact stage reuse/identity conflict |
| P1 | Native row issuance repeats per target; current-source association still becomes a boolean, scan and Unknown | One physical row issuer, exact source association through installation |
| P1 | Ordinary program and package selection remain independently constructible | One closed target reference, including ordered imports |
| P2 | H narrowing restores the entire parent's physical custody | Merge only required native contribution custody |
| P1 detection | Several test premises are wrong; changed test consumers were not all compiled | Correct production fixtures and compile affected test targets early |

### Private history is not one program

For birth B, program one produces C; program two correctly starts at C and
produces D. `CompilerWorld::item_successor` stamps the local cursor's
`original_base=C` (compiler_world.rs:2050). Freeze compares it with B
(paired_publication.rs:662; compiler_world.rs:1684,1798), and promotion repeats the
comparison (:2144). The extended abandonment test runs precisely this history.
It is a latent one-program assumption exposed by the current-state repair,
not evidence that retirement itself caused the failure.

Do not overwrite the cursor's base with B or loosen equality. Keep local cursor
checks and issue one cumulative settled frontier from the private owner. See
[PRIVATE-STATE](PRIVATE-STATE.md).

Further source-confirmed defects: begin_freeze ignores Active (admission.rs:953);
no-row reconciliation returns without a coherent revision recheck (:4654);
snapshot issuance can capture r+1/r (:4761); begin_world_cell_program claims during
preparation (:4450). Activation registers twice (resident.rs:6275,6317).
Actor refusal paths consume cleanup custody while only logging failed cleanup
(resident_workbench.rs:5427–5514). The N patch addresses portions but is not safe
or compilation-ready as preserved.

### Job authority is absent before Rust reads it

The retained compiler's `turn.cbor` and `world-item.cbor` both contain a null
authority for job1. Its exact root is main/Tidepool.Command.Types.Job. The
retained source SHA matches the shipped Types.hs; prepared Job has one strict
lifted-reference field. Those observations rule out Rust dropping a non-null tag
and provide no evidence for changing Job's runtime layout.

A concrete migration omission exists: GhcPipeline.hs:4950 supplies only
`scopeCanonicalInterfaces (pvExactScope variant)` to typed binding preparation,
but the World path prohibits pvExactScope (:5291). The callback therefore receives
an empty map even when authenticated World evidence exists. The callback runs
after selected World proofs are available. Source-less Finder resolution in
ExactScope.hs:907 needs that proof.

The exact failed Finder/representation predicate in the retained run remains
unobserved. Instrument that bounded decision and fix the evidence route; do not
grant authority by module spelling. TYPE-only source-origin proofs must work
without granting SOURCE execution. See [COMPILER-EVIDENCE](COMPILER-EVIDENCE.md).

### Checked Text is an inter-stage certification failure

The executable item artifact is written only after successful segment
certification. The following declaration receives that prior certificate.
The generic refusal in CertifiedProducts.hs:597 conflates fresh source omission,
prior seal/version/native-owner conflicts, binder duplication and related cases.
The log does not prove duplicate binders. Recovered source emitted again at the
next stage is a concrete lead, not yet the established failing predicate.
Split the refusal, then rerun this exact mixed cell before changing admission.

### Native/package implementation remains incomplete downstream

Repeated finite-target certification creates new pending group/import owners.
Completion compensates with `same_exact_rows` and substitutes the earlier product
(compiler_projection.rs:1163–1198); another escape exists in artifacts.rs:6451.
This is restricted to the same exact NativeGroupBinding. Do not misreport it as
the old arbitrary owner/ordinal substitution. Fix the physical cohort issuer,
then delete the equality repair.

Current-source evidence becomes a boolean, is found again by a row scan, and can
become Unknown (artifacts.rs:3612; turn.rs:1693; prepared/closed.rs:647).
The installer still accepts that route. P's preserved patch is promising here,
but unqualified and still retains row reissuance. Ordinary target/program pairing
also retains a raw-links constructor and separate accepted-import argument.
[PACKAGE-CLOSEOUT](PACKAGE-CLOSEOUT.md) assigns these concrete cuts.

H's contribution helper returns the original view for no contributions, or
extends that original view. Merging it into a narrowed view restores unrelated
physical payloads (artifact_inventory.rs:8068). Receiver roles and executable
roots remain restricted; **no permission escalation is demonstrated**. The
existing sibling test starts already narrowed, so misses the retained-memory bug.

## Test results need a different interpretation

- The nine-case joined actor log records 5 pass/4 fail. It started before the
  final code merge and does not retain an exact source OID. It is useful failure
  evidence, not exact-29231 qualification.
- Equal-content package fixture recursively rebuilds Held children as Planned
  content IDs. The allegedly distinct dependency histories become the same
  graph. Preserve actual Held children before asserting distinct issuance.
- Transitive HOME fixture requires the local target to be a dependency HOME row.
  Actual optimized output is target -> Middle -> Leaf -> package, and the fresh
  manifest contains the required Leaf package roots. Repair the oracle; do not
  rebuild the producer on that assertion alone.
- Cached item fixture counts an import plus a binding as one item. Establish the
  actual plan shape and lexical environment; do not merely change an integer.
- Failed-later-item fixture is rejected during typechecking, before effects.
  Unmerged 7fac7d7603 tries an annotation but leaves literal `Eff {}` in a plain
  string. Use the actual effect alias and prove first-item runtime progress.
- ExecutionProjection's changed signature still has an unmigrated
  PreparedRecoveryTest.hs:1073 caller. Production compilation did not cover it.
- The five notebook installation failures were from T's branch **without P's
  transitive repair**. Retained executable/worker hashes establish that distinction.
  They show a demanded HOME-link omission in that older producer, not a proven
  current-checkpoint defect. Rerun the joined registered cohort early.
- Source-race WIP replaces the World intent with binding-only publication and
  does not establish unchanged private A. It cannot qualify the required race.

## WIP disposition and path forward

Preserve all three patches under `g2-next-wave-90a54565/{p,t,n}/`.
Reuse reviewed hunks, not entire patches blindly. P source-association work,
N first-entry ownership/activation guard direction, and T cached lexical fixture
are useful. Correct their identified gaps and compile the joined consumers.
Keep earlier H/D diagnostic work preserved; no blanket deletion or merge.

[NEXT-WAVE](NEXT-WAVE.md) replaces the old assignments.
[TEST-GATES](TEST-GATES.md) controls the next discriminators.
Ordinary activation, successful discovery/default reload, and compiler
disposal/reacquisition remain G2. Full force/fork/dead-process recovery matrices
remain G3. Neither phase is accepted by this review.
