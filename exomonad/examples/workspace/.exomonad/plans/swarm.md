# Scaffold, fork, fold

Sol/Astra supplies an obligation, acceptance criteria, shared invariants and
enough decomposition guidance for bounded implementation. A Luna works on that
obligation through role-specific actions. It does not write notebook Haskell.

A worker either submits its completed revision or writes a scaffold, commits
it and starts children. Every child starts from that same scaffold commit in
an isolated worktree. A child can repeat this operation. After its children
settle, the parent inspects the integrated result and decides whether to
scaffold another batch or submit its subtree to its own parent.

The worker-facing actions are deliberately small:

- **Scaffold and fork:** checkpoint the authored scaffold once, supply the
  child obligations and optional early frontier, then park. The runtime
  observes the checkpoint SHA and reports any partial launch admission.
- **Submit:** publish the observed committed candidate to checks and review.
- **Ask parent:** return a concrete contract question with its evidence.

The reviewer returns accept, local repair, or a contract question. It cannot
change the submitted SHA by returning a different candidate. Tool bindings
supply the request ticket; Luna supplies only the work and judgment.

`Project.Swarm` specifies one batch of this recursive process. The supervisor
chooses the task type; the protocol does not require a role hierarchy, a fixed
whole-tree representation or a second format for acceptance criteria.

```haskell
import qualified Project.Swarm as Swarm

Swarm.begin (Swarm.Limits { Swarm.repairLimit = 2, Swarm.rebaseLimit = 2 })
  Swarm.AllChildren scaffoldCommit
  [("parser", parserTask), ("consumers", consumerTask)]
```

The result contains retained state and requests. `advance child ticket reply`
consumes an operation's reply and returns the next state and requests. Ticket
correlation binds checks and review to the dispatched candidate. Duplicate or
stale replies do not authorize another operation.

## Routine work

The driver launches all `Work` requests without waiting for a sibling. Work
may include its own recursive batches. Its reply is the worktree head observed
by the existing submission owner, not a SHA invented in a model response.

`CheckAndReview` runs the prescribed checks at that candidate, then starts a
fresh Luna against the original obligation and its enclosing invariants. An
acceptance advances that exact candidate. A concrete implementation finding
starts a bounded repair followed by new checks and fresh review. An incomplete
or conflicting contract goes to the parent. Review may identify bugs absent
from the acceptance text; there is no vote or criterion-key exclusion rule.

One integration operation runs at a time for a parent worktree. `Merge`
receives the expected parent head and exact reviewed candidate. The existing
integration owner observes Git and runs the configured checks for that merge.
The driver reports `Applied` only after those checks pass. If a rebase is
required, a worker receives both revisions; its replacement goes through
checks and fresh review before merging. Retry counters belong to the child.
If integration checks fail after a known merge, further merges pause while
`RepairIntegration` and `CheckIntegration` repair and review that head against
the enclosing node's integration contract. These repairs share the child's
repair budget. Acceptance resumes the fold; a contract question or exhausted
budget wakes the parent with the observed head retained.

Every successful merge retains a `ChildMerged` receipt containing the child,
reviewed candidate and resulting parent head. `QueueNotice` presents that
receipt without starting inference. `BaseAdvanced` updates a sibling's known
integration base without asking the parent to forward a prose instruction.
It does not mutate an active worker's checkout or invalidate an in-flight
build. Rebasing, when needed, is an explicit operation after submission.

The default frontier is all children merged or escalated. `After` names a
nonempty subset that can unlock the next scaffold earlier; the final batch
settlement still requests a wake. Routine merges cause no other wake. A
contract question wakes promptly while unaffected children continue.

Wakes may coalesce. At activation the parent reads the current `view`: current
integrated head, child states and retained merge receipts. An old notification
is never the authoritative snapshot. The parent spends this turn inspecting
the result and choosing its next scaffold, rather than relaying receipts.

## Exomonad binding

This is executable Haskell policy, not a new Git or process interpreter. A
record actor retains the `Batch`, dispatches requests and receives typed
settlements. Child assignments use `report = Silent`; `R.settlement`,
`R.forwardResult` or the existing watch routes continue the workflow without
model inference. Store merge receipts in actor state. Call `sendMessage` only
for `WakeParent`: it is a normal inference notification, not a quiet queue.

Run integration in the principal that owns the parent worktree. A record
actor's effect row does not grant ownership of a sibling's checkout. Reuse
`tryMerge`, submission observations, request admission, process supervision
and build-resource ownership. Keep the shared context prefix for workers;
fresh review gets the contract and exact candidate independently of the
implementer's conversation.

Persist state and admitted-operation handles before releasing the callback
that owns them. Compaction must not lose the tree's obligations or dispatch
them twice. Process death or an uncertain merge requires reconciliation by
the existing owner before resuming; the policy does not replay external
operations from a log. Its tickets are scoped to one batch mailbox.

An integration check failure records the actual resulting head and pauses
further merges during repair. An uncertain merge escalates immediately,
retaining the last known head. The owner reconciles that external operation
before commissioning another batch; uncertainty never triggers a blind retry.
Likewise a frontier containing an escalated child means its work has settled,
not that the dependency succeeded. The parent sees that distinction in `view`.

## Implementation boundary

The pure policy and its command/reply scenarios execute under pinned GHC.
Binding the operations to role-specific native worker tools remains Exomonad
integration work. The protocol specifies what those tools must do; it does
not claim that a new worker tool or a live swarm has been installed. Parent
source changes at an early frontier must be serialized through the same
integration owner, which checks the actual head before a later merge.

Run the policy scenarios without launching models:

```sh
bash scripts/dev-shell.sh runghc -Wall -Werror -iexomonad/examples/workspace/.exomonad exomonad/examples/workspace/.exomonad/tests/SwarmSpec.hs
```
