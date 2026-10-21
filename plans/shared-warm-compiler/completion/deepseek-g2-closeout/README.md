# DeepSeek G2 closeout: B, with C investigated in parallel

Accepted direction, 2026-10-21. This is the controlling **next-wave work pack**.
The user selected G2 correctness plus bounded boundary/quality cleanup, with
producer-demand simplification investigated alongside it. Cheap inference is a
reason to parallelize independent work and review; it is not evidence that a
candidate is correct or that cold-build capacity is unlimited.

Start with [STATE](STATE.md), [the shared contracts](CONTRACTS.md), and
[the supervisor runbook](SUPERVISOR.md). The existing
[G2 acceptance contract](../g2-completion.md) still applies. This pack supersedes
the older takeover's task assignments, status and no-format-change assumption.
It does not supersede exact historical dependencies, cancellation, publication,
authority or cleanup requirements.

## Outcome

Consolidate and publish a reviewed G2 candidate, then qualify one matched frozen
bundle and a fresh DeepSeek Persona conversation. Preserve Persona's authored
files and durable memory. Verify the actual Persona spec, tools and rendered
initial memory; never substitute a generic root or stale binary.

There are **three independent correctness fronts**:

1. Package products lose the relation between a particular consumer and its
   selected package occurrence before Rust admission.
2. A retained interface copy is mistaken for a new package selection because
   its storage path differs from the original observation.
3. Publication demands a checked-cell prefix after an installed native tool
   invocation that did not compile a cell.

The defining-Name candidate is an approved prerequisite, not a solution to all
three. Existing closed-support, HOME census, affine initialization and logging
work should be retained, not reimplemented.

## Worker tree

Use DeepSeek workers. The caller is expected to launch them in its own harness;
this plan's existence does not mean any worker has been launched. Aim for
12–16 productive workers plus the campaign root when the harness permits it.
Queue lower-priority bodies if capacity is smaller. One task owner may spawn
bounded helpers, but register them before dispatch so the total remains visible.

| Role | Primary work | Parent / start condition |
|---|---|---|
| S campaign | Integration, technical judgment, resource admission, release | Root; [runbook](SUPERVISOR.md) |
| P join | Freeze the package contract; consolidate producer, consumer and controls | S; [package plan](P-package-linkage.md) |
| P1 producer | Preserve target and fresh-HOME package occurrence choices | P; trace immediately, edit after contract freeze |
| P2 consumer | Decode exact links and use existing issued graphs | P; trace immediately, edit after contract freeze |
| P3 oracle/review | Independent producer histories and cross-language review | P; design controls immediately |
| W interface | Distinguish selected interface from retained copy | S; [witness plan](W-interface-witness.md), immediate |
| N publication | Carry actual compiler progress through native publication | S; [publication plan](N-publication.md), immediate |
| Q1 diagnostics | Logging contract, transport, partial-acquire test cleanup | S; [quality plan](Q-quality.md), immediate |
| Q2 census | Remove full-payload comparison from census maintenance | S; quality plan, immediate |
| Q3 trace | Remove repeated per-owner diagnostic graph scans | S; quality plan, immediate |
| Q4 Original | Delete obsolete Rust facade-authority reconstruction | S; after structural candidate lands |
| H histories | Preserve D and finish real cold/eviction/reacquisition controls | S; [history plan](H-histories.md), immediate |
| C1 schema | Compare the actual package metadata meanings independently | S; [C investigation](C-producer-demand.md), read-only first |
| C2 demand | Trace why that metadata was demanded and who consumes it | S; C investigation, read-only first |
| R boundary | Independent W/N and joined-authority review | S; trace now, review exact candidates as they arrive |
| R resources | Storage inventory, retention decisions and scoped cleanup | S; runbook, immediate |
| V qualification | Prepare exact commands, run joined gates, reconcile map | S; [qualification](V-qualification.md), prepare immediately |

P is a working supervisor: it reads the producer/consumer join, resolves the
contract and integrates its subtree. S directly owns the small quality leaves;
do not add a supervisor whose only work is forwarding status. Reuse completed
Q/C workers as independent reviewers of other candidates. A worker cannot
independently approve its own implementation or test oracle.

## Delivery sequence

1. Verify STATE, preserve D's dirty work, start the independent frontier, merge
   the reviewed defining-Name candidate into the campaign branch as a distinct
   prerequisite, and record the package wire/identity contract.
2. P consolidates the cross-language package change. W and N proceed independently.
   Integrate Q/H parcels as reviewed; do not hold unrelated finished work behind P.
3. Compile affected production and test consumers early. Replay the actual
   discriminating histories; run Discovery and all four notebook cases on the
   joined source. A later failure is new information, not a passing test.
4. Close required G2 histories, independently review the joined consumers,
   reconcile the map, and freeze/qualify one bundle. Publish exact GitHub refs.
5. Start the fresh Persona run from that bundle. Keep G3 obligations and any C
   proposal separate and explicit.

C is not a release prerequisite. S may admit one small, independently proved
C repair before qualification freezes; otherwise retain its proposal, evidence
and branch for the next tranche. Do not restart qualification repeatedly for
new speculative improvements.

## What good work looks like

Pass the owner's decision forward instead of rediscovering it. Make coupled
facts one value; keep custody, selection and execution distinct. Delete replaced
routes. Prefer an existing typed owner and one production path over another
registry, framework, flag combination or test-only issuer. Optimize work that
has no consumer before optimizing its lookup. Retain complete original failures
and cleanup uncertainty; render concise diagnostics at the reporting boundary.

Each pass has one primary goal and two or three adjacent questions. Record
incidental findings with source and evidence, but do not silently widen its
implementation. No new research is required to rediscover facts already proved
in [the reviewed evidence index](STATE.md).
