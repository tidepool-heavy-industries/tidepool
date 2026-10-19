# Overnight execution: low-effort root, supervised engineering

This is the root coordinator's short operating brief. The user intends to run
this coordinator as Sol low with a user-started goal loop. A Sol-high campaign
supervisor owns engineering strategy and exact-candidate review through the
tree. Root monitors progress, routes decisions, maintains the global capacity
view and ensures useful work is integrated and recoverable.

The user's explicit choice of a native goal loop overrides the repository's
general preference against native Codex goals for this campaign. Do not create
another continuation service or start a goal without the user's instruction.

## Branch from the release, not the research checkout

At execution kickoff refresh the published release reference deliberately and
record the full base OID. Agree the receiving branch/push owner with the parent
delivery session. Create `campaign/shared-warm-compiler` from that **latest
release tip**, then isolated area and task branches from reviewed dependencies.
Carry this documentation commit into the campaign without switching an active
checkout.

Read-only remote inspection during planning found:

- Published `release/overnight-consolidation-20261017`:
  `545d9950e5c3c3940f5b6bf84dfc5d16d6b1a89d`.
- Published peer integration `integration/harness-threshold-20261019`:
  `70a92c65a02a591f76590daf4e5bfa3e6fcbf723`.

These are observations, not instructions to pin a stale checkout. The local
`origin` is another checkout; `github` is the public remote in the plan clone.
Verify remotes before fetching or pushing. A locally tracked release ref was
older than the published tip. Do not equate remote nickname, branch date or the
research checkout's HEAD with the current release.

Before measuring or rewriting, verify the release contains the peer's optional
root selection repair and required follow-up tests. If it does not, fold the
reviewed peer delta with its owner first and record that new campaign baseline.
Do not drop the repair just to start from a branch named release. Preserve the
stronger Persona test's actual result, including any later correction.

## Model allocation

| Role | Default | Reason |
| --- | --- | --- |
| Root coordinator | `gpt-6.1-sol`, low | Observe, route, account, wait and close the frontier |
| Campaign supervisor | `gpt-6.1-sol`, high | Shared contracts, design judgment, cumulative integration |
| Area supervisors | Sol high for input semantics/GHC/reload; medium otherwise | Review and sequence their actual production seams |
| Bounded implementers | `gpt-6.1-sol`, medium | Known contracts, concrete owned consumers and tests |
| Difficult implementation or independent semantic review | `gpt-6.1-sol`, high | Ownership, GHC lazy state, generation races and independent oracles |
| Rote helpers | `gpt-6-luna`, low or medium | Source inventories, caller/fixture/link audits, mechanical documentation or evidence indexing |

Use actual supported model/effort identifiers and record them. In the current
collaboration API an explicit model/effort override requires `fork_turns="none"`;
therefore give the child its complete assignment packet and document paths.
Do not rely on inherited conversation for shared decisions.

Luna findings are inputs to Sol review. Do not assign Luna shared architecture,
proof issuance, cancellation/retirement design or final semantic acceptance.
Helpers consume the same global slot budget; use them when their independent
output saves effort, not simply because a slot exists. Preserve the normal
15-active/hard-17 total in [SUPERVISION.md](SUPERVISION.md).

## What the root does

1. Give the [campaign assignment](supervisor/README.md) to a Sol-high supervisor,
   with the agreed release base, plan commit and available capacity.
2. Inspect actual active workers, dependency readiness, review/merge queues and
   build/resource ownership. Prompt the responsible supervisor when a concrete
   next action waits on it. Prefer reusing an informed worker to recreating context.
3. Route bounded fixes and observations through the campaign supervisor. Do not
   substitute a low-effort guess for an unresolved semantic contract.
4. Require timely leaf-to-area-to-campaign folds and exact evidence. Qualified
   checkpoints can be handed back to the release owner during the night through
   the agreed integration path. Do not wait for every experiment to finish.
5. When only other agents/builds can advance work, wait on their events. Do not
   repeatedly poll unchanged status, create empty commits or invent new tasks.

Useful progress is an accepted decision, implemented consumer, discriminating
test, repaired failure, reviewed fold or measured result. A growing number of
agents or documents is not progress by itself.

## Hard strategy questions: drain, then terminate

The user explicitly wants this terminal rule:

> If a genuinely hard implementation-strategy question cannot be resolved
> within the accepted plan, finish the rest of the unblocked frontier, then
> terminate the goal loop for morning review.

The campaign supervisor first uses the relevant Sol-high context and a bounded
independent review to distinguish an ordinary bug from an actual strategy gap.
Do not escalate a routine compiler error or a settled contract's implementation
detail merely because it is inconvenient. Also do not conceal a strategy gap
with a speculative fallback that weakens the invariants.

For a hard gap:

1. Record the minimal counterexample, owning source/commit, evidence, attempted
   explanations, plausible options and the exact decision needed. Use
   `MORNING.md` in this directory during execution.
2. Close only its dependency cone. Stand down its idle workers with clean
   committed feature branches and explicit status; preserve safe in-flight
   ownership/cleanup observations.
3. Finish all work already approved and still independent, including its
   review, repairs, feature/campaign folds and applicable tests. This is a finite
   frontier, including approved successors that become ready as prior waves fold;
   it is not just the batch runnable when the question appeared. Do not launch
   speculative successor campaigns to keep the loop alive.
4. Before terminating, reconcile every worker, candidate and admitted build:
   integrated, explicitly archived, awaiting the named morning decision, or
   retained with an exact external blocker/continuation. Capture outstanding
   uncertainty truthfully; never clear resource ownership just to make the list empty.
   Campaign-owned running tests/builds and pending actionable reviews remain
   frontier work to await and finish. They are not external blockers that permit
   premature termination. Unrelated parent services are outside this frontier.
5. Write the morning handoff and terminate the user-started goal loop once no
   approved runnable work remains. Do not leave it waking repeatedly to restate
   the same question. Do not call the compiler campaign complete if it is not.

Define the overnight continuation objective as **draining the approved runnable
frontier into reviewed integrated checkpoints and a decision-ready morning
handoff**, rather than promising the entire research campaign unconditionally.
Completion of that bounded overnight objective is distinct from full campaign
qualification. Goal status must remain truthful to the actual objective and
the active continuation tool's rules.

## Suggested user goal text

```text
Read /srv/swarm/checkouts/tidepool-shared-warm-plan-20261019/plans/shared-warm-compiler/OVERNIGHT.md
and execute that plan from the latest coordinated release branch. Run a Sol-high
campaign supervisor with Sol medium/high area
workers and optional Luna rote helpers. Implement the accepted plan in reviewed
waves, fold leaves into area feature branches and accepted checkpoints into the
campaign branch, and verify through the stated production paths. Wait on workers
and builds instead of spinning. If a genuinely hard strategy question remains,
close its dependency cone, finish all other already approved unblocked work,
record exact integrated state, evidence and morning decisions, then terminate
the goal loop. Do not claim full campaign completion for a partial handoff.
```

## Morning return packet

Provide the campaign/area/base OIDs and branch containment; what is merged and
qualified; exact tested bundle and performance evidence; compiler-map revision;
all unreviewed/unmerged dispositions; active or uncertain resource ownership;
and the smallest set of architecture decisions that would reopen blocked work.
State whether a new Persona/coding bundle is testable. Deployment is a separately
coordinated action, not implied by a successful compiler test.
