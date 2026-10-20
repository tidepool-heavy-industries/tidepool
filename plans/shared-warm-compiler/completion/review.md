# Completion-plan review and evidence limits

Reviewed source: `6f7ae09b3bdbcfd8952d9a513db91564ea1b5858`, latest code
`92fa518105`, branch `integration/compiler-query-warm-final`, checkout
`/srv/swarm/checkouts/tidepool-join-final-20261019`.

The branch was clean before this planning pass and its existing checkpoint was
pushed to GitHub. `origin` in this checkout is a local plan repository; `github`
is the public Tidepool repository. Read-only GitHub observation found main
`545d9950e5c3c3940f5b6bf84dfc5d16d6b1a89d` ancestral to this join. Refresh
that observation at implementation/final integration; do not treat it as a lock.
The integration-status report found matching four Harness pins at
`1893773e14b02b3015d99ba63114decfd3223949`; strict status reported
`local_main_missing`. Branch publication does not establish main qualification.

## Audit scope

Five Sol reviewers investigated independent seams and discussed adversarial
counterexamples with the staff coordinator. The coordinator also read the
production consumers and retained evidence, then edited the shared contracts
and delivery sequence. These are source/evidence reviews, not new execution.

| Primary review | Adjacent checks | Result captured in |
| --- | --- | --- |
| Exact native selection | Birth/current projection, accepted candidates, retained A1c failure | [native-selection](native-selection/plan.md) |
| Warm cells and lifetime | Actual actor caller, successor ownership, finite deletion, bounded cache | [service-cells](service-cells/plan.md), [runtime-publication](runtime-publication/plan.md) |
| Source/reload ownership | Preview state machine, cleanup/caller loss, actual force retirement | [lifecycle](lifecycle/plan.md) |
| Test oracles | Every recorded red case, fake authority/merge/recovery setup, meaningful work/lifetime assertions | [tests](tests/plan.md) |
| Query surface | Real GHC versus runtime snapshots, stale views, actual executed public example | [queries](queries/plan.md) |

The implementation scope was resolved with the user: complete the mandatory
original campaign, deliver a reliable early Persona candidate on its way, delete
the old resident engine and port valuable tests through production APIs. Existing
native/runtime authority remains required; deleting compiler plumbing is not
permission to delete its genuine issuance, custody or settlement guarantees.

## Corrections to prior readiness claims

- Persistent GHC and real compiler queries already exist. This is not a blanket
  replacement of a new process per cell. Ordinary actor cells nevertheless still
  use finite preparation; the new executable successor path is incomplete.
- `admit_world_cell_program` and `retainHeldWorldSuccessor` have no production
  callers at the audited code. Runtime item/final world publication refuses
  missing capabilities. An earlier hosted successor pass therefore does not
  establish the desired migration.
- The prepared introspection fixture executes `pure 43`, not its all-five effect
  composition. Dispatch accepts `Right` envelopes without proving all useful
  semantics. `currentScopeView` is a snapshot, not a universal reply-time fence.
- The A1c initial example reaches its first display assertion. A later structured
  probe fails on an exact native dependency. Its diagnostic metadata is not a
  complete native custody inventory, and root-E is not established as its cause.
- Joined reload has a stronger affine preview state than the stale follow-up.
  Preserve that state while forward-porting missing original-A/custody behavior;
  do not merge the old branch wholesale.
- The 32 toolchain failures are not 32 proved product defects. Several fixtures
  manufacture authority or use invalid oracles; later failures remain hidden by
  the first assertion. Neither green counts from weakened parcels nor deleting
  every failure without preserving its distinct contract is accepted.

## Cross-plan review and changes applied

After the subplans were written, reviewers checked other areas' consumers and
qualification, rather than only rereading their own work. The following changes
were incorporated before publication:

1. **Reload world coupling:** frozen B's owner-issued logical compiler world and
   restoration roots travel with candidate custody, atomic generation publication
   and cold recovery. Source/tools/notebook alone are insufficient; draft C cannot
   reconstruct B.
2. **Cleanup classification:** a known reader-busy refusal returns the same opaque
   custody and a settled typed cause. Only unobserved/dispatched/lost/panicking
   physical settlement is uncertainty. Primary cause and independent secondary
   cleanup/storage obligations jointly govern recovery and retryability.
3. **Retirement set:** active generation-dependent work, its required immutable
   custody and the collateral physical closure of proposed stops are distinct.
   Inert historical values/source/receipts do not block reload solely by existing.
4. **Native/service handoff:** authenticated lexical/type/instance/scaffold facts
   come through the existing output/source owner; native policy resolves exact
   edges and captures birth. Name shared-function owners and serialize signature
   commits. Neither leaf reconstructs the other's closure or adds a certifier.
5. **Historical offers:** removing wholesale cold-parent borrowing must still
   permit a demanded historical offer from its actual issuer. Compiling a new
   product cannot substitute for a missing exact historical binding. Mandatory
   dependency reuse cannot be waived by always recompiling.
6. **Executable verification commands:** owned-resident ordinary-demand testing
   explicitly supplies retained output and the full trace profile it requires.
   Property scale starts with bounded feedback before measured expansion.
7. **Stable qualification inputs:** refresh remote tracking refs and verify pins;
   do not upgrade every dependency during final qualification. A deployment path
   may use `fast-dev`; claims bind actual profile/package bytes.

The reviewers found no planned second evaluator/certifier, legacy fallback or
required lock held over compiler/shutdown waits. This is acceptance of the written
contracts, not proof that a future implementation obeys them. Concrete candidate
diffs, real consumer compilation and joined tests still need independent review.

## Open investigation, not unresolved permission

A1c's exact lost binding, the actual accepted-candidate receipt, several deeper
red-case identities/materialization outcomes, the incremental delta encoding and
resource-specific retirement acknowledgements need implementation investigation.
The plans specify discrimination controls, owners and escalation conditions.
Representation choices may be made locally within the locked contracts. A real
contract conflict is escalated with a counterexample while independent work
continues; it is never solved by resurrecting finite execution or forging proof.

Original evidence remains under
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/`, including
`WORKTREE-MAP.md`, `reviews/`, `final-join/TOOLCHAIN-SWEEP.md` and retained failed
compiler inputs. Dirty diagnostic and unmerged parcel checkouts remain intact.
Plan validation checks Markdown references, whitespace, unique 32-case coverage
and consistency with reviewed source. No builds, tests, live launches, storage
retirement or new performance claims are part of this documentation commit.
