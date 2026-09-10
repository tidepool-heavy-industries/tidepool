# Compose and continue application work

Read your assigned plan and the relevant recipe below. For recursive parallel
implementation, start with [local waves](operating.md#repeated-local-waves-and-context-choices).
Only the initial designated leads submit execution plans for planner review;
descendants implement within that agreement unless their assignment says otherwise.
The optional [graph walkthrough](graph/run.md) illustrates a separate project.

Launch preparation and package checks belong to the launch operator; workers
start with their assigned product work. See [launch.md](launch.md) when preparing
a new swarm or validating a package change.

## Context and source defaults

`solTask` and `componentLead` inherit the current completed reasoning and use
`boundHead`. `implement` uses `solTask`. They suit recursive implementation from
an allocated checkout. Component leads, solTask/implement and reviews explicitly select Medium;
keep Sol effort stable across inherited forks. Original-root callers use `solTaskFrom label projectHead`
or `componentLeadFrom label projectHead`. A Task's source hash records provenance;
it does not override that live checkout selection. Commit coherent work for Git
integration and restart recovery, even though ordinary unfold inherits working files.

Use `withContext (selected taskContext)` for an independent component or fresh
inspection; use `atRef (GitRef source)` with the source variant for an exact
committed checkout. Context/source/model/lifetime stay separate choices. Review
helpers deliberately select fresh context and the exact candidate commit.

## Commission the current component

The workbench accepts an ordinary Task for any project. Prefer a constructor from
the current plan when it supplies one. Otherwise bind `plan`, `source`, `outcome`,
`why`, `paths`, `criterion` and `decisions` from the agreed component contract:
plan path, exact committed Git hash, owned result, rationale, owned paths,
acceptance and incorporated decisions. The text fields are Text; paths and
decisions are lists. Do not put an explanatory sentence in `source`.
Capture the current Sol owner's address before creating a router. Its policy
runs as the router, not as the capturing model.

```haskell
import qualified Tidepool.Actor as Actor
let Right campaign = campaignLabel "current-goal"
let Right owners = forkGroupLabel "owners"
let Right label = branchLabel "component-a"
let group = batch campaign owners
let task = Task group plan source outcome why paths criterion decisions
work <- unfold group (childWithProgress @WorkProgress @Delivery (withContext (selected taskContext) (componentLeadFrom label projectHead task)))
let (lead, progress) = work
owner <- actorContext
wave <- followWork [("component-a", forkedResponse lead, progress)] (notifyWork owner (withCheckpoints (workMessage deliverySummary)))
```

Continue the parent's independent engineering after attaching the wave router.
It retains progress and the terminal receipt, and messages only actionable deltas.
End the turn when progress depends on those results. On wake, query
`view <- readWork wave`; inspect the relevant `collectedWork view`
entry and its `sourceResult`, preserving the full receipt for checks. A fresh label is needed when prior
branches reserve the example names. This uses ordinary supervised lifetime; a root
can deliberately choose SwarmOwned for selected leads that should outlive it.
Choose that independently from the task, source and model. Review/repair and later
local frontiers use the same operations below and in [operating.md](operating.md).

## Implement, review, repair in the context that owns the code

A lead implements substantial work and owns its recursive local waves. After
scaffold/fork/integration, bind the exact checked commit as candidate:

```haskell
(reviewer, progress) <- reviewCandidate task OwnerRepairs candidate
owner <- actorContext
reviewWave <- followWork [("review", forkedResponse reviewer, progress)] (notifyWork owner (workMessage reviewSummary))
```

The reviewer returns Produced (Repair latest findings) for defects the lead must
repair. Its attempt settles; the lead's delivery stays open. After local repair and
checks, bind `revised :: Candidate` and reuse the retained reviewer:

```haskell
let Right retryLabel = requestLabel "review-repaired"
(attempt, retryProgress) <- reviewAgain (forkedActor reviewer) retryLabel (ReviewTask task revised OwnerRepairs)
retryWave <- followWork [("review", attempt, retryProgress)] (notifyWork owner (workMessage reviewSummary))
```

Retain the prior attempt's receipt and any unanswered questions before replacing
it with the next attempt. When the prior result is incorporated and every remaining
obligation has an owner, drain that old router:

```haskell
previousAttempt <- finishWork reviewWave
```

The next attempt has new source handles, so it gets a new router. `replaceActor`
repairs behavior for the same sources; it does not advance a wave. Closure alone
is no reason to discard unresolved questions. The final state remains in
`previousAttempt`; retain useful reviewer agents separately from these collectors.

The reviewer incorporates that revision before checking it. Keep its latest
accepted Task/Candidate intact. A reviewed head and the lead's resulting checked
head are different facts. Once the lead has checked its resulting checkout:

```haskell
respond (Produced (Delivered accepted head checks))
```

Here accepted is the actual ReviewedCandidate, and head/checks describe resulting
source. Keep gates in its nested candidate. Review semantic integration changes.
Blocked is an honest terminal product result when the obligation cannot continue.

Open broad independent implementation frontiers when a usable scaffold makes
them productive, repeating the pattern inside substantial children. `implement
part` returns `(Forked (Outcome Candidate), Progress WorkProgress)`. Attach
its response and progress to the local wave router. After
that worker returns, reviewCandidate part (RetainedImplementer (forkedActor worker))
latest lets the reviewer request repairs directly. The worker is then available;
queuing repairs behind a lead's pending delivery would deadlock it.

For a known, already-authorized handoff to an available reviewer, use the
[typed continuation example](continuation.md). It submits the settled candidate,
collects review events and returns typed values to its owner's mailbox without a
model relay. The owner still decides acceptance, repairs and source integration.
Do not automate a consequential decision by treating every terminal value as success.

## Questions stay open until the owning decision arrives

When activation supplies reportProgress, bind `open :: Attention` to cumulative
unresolved questions and `question :: Question` to the concrete finding:

```haskell
let updatedQuestions = raiseQuestion question open
reportProgress (WorkProgress [candidate] updatedQuestions)
```

A stable questionKey is local to its plan; source and finding distinguish revisions
of that question. Publish the whole unresolved set so a newly attached collector receives the
current questions. Attached actors receive every subsequent publication. Publish on meaningful changes, not every tool step. Keep this request
pending and continue unrelated useful work. The Sol owner handles the question; this is not a planner notification.
The wave router retains evidence and each source's question set, and selects
which changes deserve a message. See [coordination.md](coordination.md) for local
retention, typed parent routing and compact notification policy.

After an owning decision, bind `decision :: AcceptedDecision` to the resolved
question, checked incorporation source, supported summary and evidence, as in operating.md. On the
request owner's retained response, for example the initial review:

```haskell
delivery <- updateRequest (forkedResponse reviewer) (decisionContext decision)
```

Handle Left explicitly; on Right retain the RequestUpdate handle and poll when
its state affects your next action. Do not spend turns repeatedly checking
presentation while independent work or checked incorporation already answers it.
UpdatePresented means the steering was presented, not that code was incorporated.
UpdateUnconfirmed/UpdateNotPresented/UpdateTooLate require examining that receipt
and current work before intervention; do not silently enqueue a replacement request.
For the retained attempt use attempt itself instead of forkedResponse reviewer.
Send through the response owner that can act; do not relay the same packet up and
down the tree merely to keep ancestors informed.

The recipient reads that supported steering, verifies incorporation and records
the typed decision in its current Task. It can then publish `WorkProgress [candidate] (resolveQuestion
decision open)`; an answer to an older version cannot clear a newer finding. Review
with the updated assignment, not the original sessionInput after its contract changed.

## Find the owning helper

Project.Work owns solTask, implement, reviewCandidate, reviewAgain, repair,
designQuestion, consultDesign, withDecision. Project.Routing owns followWork, workDefinition and notification
policy. Project.Plan owns
componentLead/componentLeadFrom and the optional graph allocation. These qualified
module names work; unqualified imports do not move a definition to another module.
Source integration uses native git merge/cherry-pick/rebase and focused checks;
there is no integrateFork or integrateCandidate Haskell operation. settledValue
is the shared public Settlement projection; retain the original receipt for evidence.

When checking several cases in one target, combine nextest filters in one owning
just invocation. Avoid restarting toolchain setup for each test or running broad
batteries after scaffolding. Compile affected targets and test the changed boundary;
run broader acceptance at integration. Never share a writable target directory
between concurrently building nodes; ordinary unfold inherits its build snapshot.
