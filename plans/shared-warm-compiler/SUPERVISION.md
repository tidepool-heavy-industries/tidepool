# Shared warm compiler supervision

This is the operating plan for a Sol campaign supervisor and recursively
delegated Sol implementation and review work. The [campaign plan](README.md)
owns the outcome, [CONTRACTS.md](CONTRACTS.md) owns shared decisions, and
[qualification/README.md](qualification/README.md) owns acceptance.
[EVIDENCE.md](EVIDENCE.md) indexes research; [STATUS.md](STATUS.md) points to
current assignments, source and evidence. The [kickoff assignment](supervisor/README.md)
starts the workflow. These documents describe intended work; they do not prove
that any agent, branch, build or service is running.

The planning base is `70a92c65a02a591f76590daf4e5bfa3e6fcbf723`. Before actual
implementation, branch from the latest release tip agreed with the parent delivery
owner and record its full OID. The research/planning branch does not own that
owner's release branch, running hosts, frozen artifacts or acceptance jobs.

## Authority and collaboration

The campaign supervisor owns shared contract arbitration, the campaign branch,
capacity assignments within the root's allowance, cross-area integration and the
final handoff. Area supervisors
own their feature branches and concrete assignments. Each implementer owns an
isolated writable checkout and candidate branch. Each reviewer owns a review of
one exact base-to-candidate change, including its production consumers and
failure paths. Use Sol high for the campaign supervisor and critical semantic
review, Sol medium/high for bounded workers and area supervision, and optional
Luna helpers for rote inventories/tracing/documentation under Sol review. The
session root may run Sol low for coordination. Follow [OVERNIGHT.md](OVERNIGHT.md)
for exact model defaults and the finite frontier/goal termination rule. Record
actual configured model/effort rather than treating an agent label as proof.

Use the actual collaboration tools available in the running session. Native
collaborator agents can spawn children, message their parent and peers, and
receive follow-up assignments. An area supervisor should create successive
implementation/review/repair waves when their independent results justify them.
A child can subdivide a concrete independent task with an explicit campaign
grant; it then owns its descendants' review and integration. Recursive spawning
does not grant recursive capacity or transfer branch ownership.

For native collaboration, `collaboration.spawn_agent` creates a working task;
`send_message` steers an existing agent and `followup_task` can activate an idle
agent. Use the tool's returned identity and canonical task path in records.
Do not simulate agents with labels or write a proposed tree as a live roster.
Hosted Exomonad actors have different semantics: they require an actual
`AgentSpec`, explicit context/workspace/permissions and a typed request after
spawn. Load the applicable agent-work skill if using that route; do not mix its
idle-actor/request lifecycle with the native collaborator API.

Peers should communicate directly about a shared consumer, counterexample or
dependency. Copy decisions and consequential evidence to the responsible parent.
A captured context does not receive later decisions automatically. A delivery
receipt is not proof that an implementer incorporated the correction.

## Capacity belongs to the whole tree

The hard session cap is **17 agents including the session root**, across every
depth and all concurrent assignments. Use **15 as the normal maximum**, leaving
room for an urgent independent review or bounded investigation. Count existing
research/other campaign agents in this same session before admitting new work.
If those agents remain live, reduce campaign grants accordingly.

The Sol-low session root owns the global capacity view and grants the campaign
an explicit allowance after counting other active work. The campaign supervisor
assigns and rotates grants within that allowance; it cannot increase it or
borrow an unrelated reserved slot. Report actual starts/settlements to the root
before capacity is transferred outside the campaign.
Use a simple Markdown capacity/ownership table in STATUS and per-area handoffs,
maintained by one serialized coordinator; this plan does not require a new
runtime scheduler, token service or orchestration schema.

Each area supervisor may have **at most two direct live children at once**,
normally one implementation owner and one independent reviewer/investigator.
Every deeper descendant consumes another explicit grant from the same total.
If the campaign supervisor is the session root, five area supervisors leave
nine leaf slots within 15. A separate session root plus campaign supervisor and
five area supervisors leave eight, before counting any other active agents.
All five areas cannot each assume two grants simultaneously at the normal cap.
Rotate reviewers and grants between areas as dependencies settle. Reuse a
suitable available collaborator through a new bounded assignment when its
previous ownership has been settled.

For every grant, record session and native agent identity, parent,
area/task, model, worktree/branch, purpose and lifecycle. Reserve capacity before
spawning; replace the reservation with the actual returned identity immediately.
An uncertain spawn remains reserved until reconciled. The normal sequence is
`reserved -> running -> settled -> released`; loss of observation is
`unreconciled`, never evidence of release. Completed dormant contexts reported
as completed by the tooling are not busy slots; keep their source/resource
ownership and the cost of resuming them recorded. Reactivating one needs an
available grant. Treat uncertain idle states conservatively until reconciled.
Interrupting an agent does not by itself retire it or release its resources.

A grant permits agent work. It is **not a build slot, memory reservation, compiler
lease or permission to start another host**. Each owner follows
[docs/swarm-builds.md](../../docs/swarm-builds.md): inspect actual process-tree
peaks, enclosing cgroups, host memory/headroom, active jobs and output leases;
prepare pinned tools and use admitted repository commands. Coordinate actual
resource or ownership conflicts. Neither token count nor CPU count proves that
several heavy builds fit. A low-space refusal is not permission to bypass build
admission. Preserve the source and exact pending command when a check is deferred.

## Unfold work into branches, then fold complete candidates

Use an agreed campaign integration branch, one area feature branch per track,
and task branches rooted at the exact accepted dependency commits. Suggested
names are `campaign/shared-warm-compiler`, `feature/shared-warm/AREA`,
`work/shared-warm/AREA/WAVE-TASK`, and `archive/shared-warm/AREA/EXPERIMENT`.
Names are organizational; full OIDs establish the source relationship.

Each simultaneous writer has a separate real Git worktree/checkout, branch,
index and provisioned Buck output. Shared object storage does not mean shared
index ownership. Reviewers use a separate checkout at the exact candidate and
avoid writes to an implementer's source/configuration/output state. Read the
root and nearest nested AGENTS/CLAUDE guides before changing an owning subsystem.
Never switch another active session's branch, stash/reset its changes, or hide
them with checkout-path operations.

For every wave:

1. The area owner records the accepted parent base OID, required dependency and
   contract revisions, paths, primary task and acceptance. Create the child task
   branch from that exact base; record a new base if dependencies change.
2. The implementation owner makes a coherent candidate, compiles affected
   production **and test consumer** targets early, executes focused counted
   cases, and commits the complete candidate history on its task branch.
   Source-only work remains explicitly unqualified. Do not wait for an entire
   campaign wave before discovering broken test callers.
3. An independent Sol reviewer examines the complete `base..candidate` diff at
   exact OIDs, the accepted contract and real consumers. Review deletion of
   alternate routes, refusal/cancellation/cleanup and the evidence's ability to
   falsify the claimed behavior. Reviewing a final repair commit alone is not
   review of the whole dependent candidate.
4. Bounded repairs stay with the owner and need no new architectural approval.
   Each new candidate OID gets a review receipt for its complete cumulative
   delta and matching compilation/necessary behavioral evidence. An old
   acceptance does not automatically cover rebased or extended code.
5. The area supervisor reads critical diffs directly, integrates accepted
   candidates into its area feature branch, and records candidate-to-area OIDs.
   Verify a small joined production history on that integrated revision before
   admitting further dependent work. Prefer preserving inspectable commit
   ancestry; if rebasing/cherry-picking is necessary, retain the full mapping
   and recheck the replacement candidate.
6. The campaign supervisor folds the complete reviewed area delta into the
   campaign branch, records area-to-campaign OIDs and rechecks joined consumers.
   Resolve conflicts as code changes with an owner, review and checks. A local
   component pass is not combined acceptance.

No accepted practical work remains only in a leaf checkout, chat or detached
patch. Preserve rejected/alternative experiments as committed, unmerged archive
branches with rationale and evidence; mark them `archived`, never `integrated`.
Archive WIP/evidence before transferring or retiring an owner. Source retention
does not authorize deleting scratch or stopping another owner's daemon. Retire
resources only through their established ownership and quiescence procedures.

Before modifying the parent's branch, coordinate the receiving owner, fresh
integration OID, intended destination and push responsibility. Deliver the
campaign's complete base-to-tip delta and evidence through that agreed path.
Check current containment/patch uniqueness through the repository's integration
status entrypoint and record intentional remote refreshes before claiming remote
freshness. Qualification and deployment remain separate recorded operations;
this plan does not authorize disturbing a live parent release.

## Dependency gates and successive waves

These are the campaign's G0–G4 dependency gates, not a demand to run all five
areas or all children at once. CONTRACTS owns the actual API/source decisions;
the table does not invent an alternate implementation boundary. Its conceptual
roles `SelectedInputs`, `CertifiedInputDelta`, `CompilerRealizationRef` and
`SourceGenerationLease` describe obligations, not a promise to add four public
types. Extend existing owners/types where they express the contract.
Qualification starts with baseline/oracle design and joins implementation
continuously. An area's numbered task waves can span several gates.

| Gate | Ready work and dependency gate | Fold/acceptance before dependent work |
| --- | --- | --- |
| G0: baseline and contract | Campaign pins the fresh coordination base, common contract, owners, negative controls and baseline evidence. Area supervisors map consumers and split bounded leaves. | Each leaf acknowledges the exact contract/dependencies; unresolved architecture blocks its dependent edits. |
| G1: exact roots and owned rows | `host-inputs` issues exact selected roots/frontdoor controls; `owned-inputs` provides admitted shared rows through its existing owner. `qualification` establishes discriminating histories and counters. Other tracks investigate their ready consumers. | Compile joined input producers/consumers and exercise an early real host-to-compiler path, including optional/required root and ownership controls. |
| G2: demand-based operation | Actual held-world query/check caller, source capture and exact cold acquisition; cell producers and lifecycle work proceed alongside it. | Real source-domain and authority controls; a warm query does not perform the old full input preparation. |
| G3: ordinary cells reuse dependencies | Warm supplies nonempty activation, Prepare/certification and retained eligible compiler answers. Host supplies exact item successors and atomic bindings/world publication. | Real bind then later use under the same actual profile/source; returned values, world transitions, strict refusals and early work/timing comparison. |
| G4: complete and qualify | Finish mutation/fork/reload/recovery and scoped retirement, delete obsolete routes, decide conditional arm C and qualify the joined bundle. | Exact source/artifact/profile evidence and claimed behavior/performance reconcile. An intermediate candidate with explicitly unsupported force is partial delivery. |

Open the next wave when its specific dependencies are ready, rather than waiting
for unrelated areas. If the contract changes, downstream work follows the update
protocol below before it is described as ready. A supervisor may collapse or
split a wave when evidence changes the critical path; retain the actual DAG and
dependency OIDs in the journal.

The [next complete workflow](README.md#next-complete-workflow) controls current
sequencing. Core dependency reuse (arm B) is part of G3. Do not defer its first
real ordinary-cell comparison until all areas finish; only deeper import-context
arm C is conditional. Report separately implementation gaps, product defects,
fixture defects and pending checks. Passing component counts do not shrink an
unimplemented consumer into a qualification task.

| Track | Primary ownership question for assignments | Two adjacent checks reusing that context |
| --- | --- | --- |
| `host-inputs` | Does the production host supply the contract's complete exact compilation inputs through their owning boundary? | Unrelated changes preserve reusable facts; changed selection/source/currentness cannot borrow stale input authority. |
| `warm-workers` | Does the existing worker/admission owner share and retain completed compatible work under the agreed lifecycle? | One waiter cancelling does not break another consumer; failure/uncertain settlement and measured resource refusal remain distinguishable. |
| `owned-inputs` | Do issued input owners carry exact bytes, selections and lifetime through the real consumer? | Equal payloads with distinct native bindings stay distinct; fork/drop/recovery preserves the surviving owner's required custody. |
| `reload` | Does replacement select one complete compatible source/worker/input state at its existing publication boundary? | Preparation failure, no-op and default refusal stop nobody; partial forced retirement reports actual stopped/uncertain owners without rollback or replay. The fence refuses new old-generation authority while retaining custody for admitted continuations. |
| `qualification` | Does the exact joined artifact demonstrate the promised user workflow and measured work elimination? | The observer catches known work/refusal controls with nonzero execution; retained resources, clean settlement and all production entry routes match the claim. |

Every leaf takes **one primary goal and two adjacent checks**, derived from its
track and narrowed to named consumers. Independent findings need evidence and a
bounded follow-up owner, not silent scope expansion. Where a reusable failure
category appears, assign a separately granted bounded analogue search while the
instance owner repairs it. Stop affected dependent work first if the violated
contract could make more concurrent patches unsound.

## Contract changes, repairs and stop conditions

Only the campaign coordinator edits shared CONTRACTS and campaign STATUS. Areas
propose changes with a minimal counterexample, violated invariant, owning
consumer, affected tasks and choices. The coordinator arbitrates shared types,
negative dependencies, identity, publication, lifetime and error distinctions.
An area's private helper is not a shared contract decision.

Publish each accepted change with a contract revision/commit, changed rule and
consumer list. Send it to every affected parent/child; each owner returns an
incorporation receipt naming the revision, candidate OID and checks or exact
pending checks. Reviewers invalidate acceptance for affected old candidates.
Message delivery alone leaves an update pending. Peers can resolve routine local
details directly, but cannot silently redefine cross-area authority.

Escalate a new architecture, public/serialized contract, owner transfer or
unresolved validity/effect model to the campaign supervisor with alternatives.
If a genuinely hard strategy question remains after bounded Sol-high review,
close its dependency cone and finish the already approved independent frontier,
including approved successors made ready by those folds. Then ask the Sol-low
root to terminate its goal loop with a morning decision packet as specified in
[OVERNIGHT.md](OVERNIGHT.md). Do not spin or expand the campaign to remain active.
Bounded implementation repairs, compilation fixes and
regressions inside an accepted contract are autonomous. Contract correctness
takes precedence over keeping agents busy. For a repair within the accepted
contract, pause the affected dependency cone, retain the counterexample, repair
the issuing boundary and detection gap, then reopen concrete tasks. A hard
strategy gap instead keeps that cone closed for morning judgment. Repeatedly
adding workers to a broken boundary is not progress.

An area is done only when its real consumers use the accepted owner, obsolete
routes/fallbacks and redundant tests/helpers are removed, affected test consumers
compile, selected tests execute a nonzero expected count, and exact-candidate
review plus a joined production scenario pass. Any intentional surviving route
needs an explicit CONTRACTS decision and owning acceptance. Compilation,
execution, performance qualification, release qualification and deployment are
separate statuses. Do not replace qualification gates with passing counts or invent
a speedup from work counters alone.

## Durable records and restart recovery

STATUS holds the current campaign/area tips, contract revision, capacity table,
next actions and pointers to per-area handoffs and private evidence. Keep
operational records in simple Markdown using the existing filesystem/Git
owners; avoid another source registry. Each wave record carries:

- session/agent/parent identity, model, assignment and grant disposition;
- worktree/branch owner, exact parent base, dependencies, candidate and integrated
  OIDs, plus archive references for rejected work;
- contract received and incorporated revisions, review findings, acceptance
  scope and required follow-up;
- exact commands, source/artifact/profile identity, selected/executed counts,
  exit status and logs; compile-only and deferred checks stay explicit;
- process/service/output ownership, unfinished jobs/uncertain settlements,
  retained evidence/products and concrete continuation.

Checkpoint before expensive work, contract publication, integration or ownership
transfer. After any restart, read the journal and inspect current Git/worktree
state, actual collaborator roster and surviving processes/services before
spawning, releasing grants or rerunning an uncertain operation. Match live
identities to the recorded session; an old agent name does not prove that its
conversation or task survived. Preserve unknown grants/resources as
`unreconciled` until inspected. If no actual tree survives, create replacements
from the recorded assignment/source/evidence and explicitly record the new
identity; do not claim resumed live descendants without observation.

Never infer successful cleanup from missing bindings, a final chat reply or a
vanished parent PID. Preserve completed action and cleanup uncertainty separately.
Every accepted candidate must have a recorded fold into an area and campaign
tip, or an explicit retained pending integration owner. For this campaign the
user explicitly selected a native goal loop for the Sol-low root; that overrides
the repository's generic preference against it. The goal loop does not replace
the durable next action, actual supervision or the finite-frontier stop rule.
