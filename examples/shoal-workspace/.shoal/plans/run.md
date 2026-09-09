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
an allocated checkout. Component leads explicitly select Medium; solTask/implement retain Low for bounded
work (override for substantial implementation). Original-root callers use `solTaskFrom label projectHead`
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
Bind `onQuestions` to the small routing policy from
[coordination.md](coordination.md#independent-progress-without-relay-turns): it
handles changed questions locally or sends consequential deltas to the Sol owner.

```haskell
let Right campaign = campaignLabel "current-goal"
let Right owners = forkGroupLabel "owners"
let Right label = branchLabel "component-a"
let group = batch campaign owners
let task = Task group plan source outcome why paths criterion decisions
work <- unfold group (childWithProgress @Attention @Delivery (withContext (selected taskContext) (componentLeadFrom label projectHead task)))
let (lead, questions) = work
let Right resultLabel = watchLabel "component-ready"
resultReady <- watch resultLabel (awaitSettledFork lead)
attention <- followAttention questions onQuestions
```

Continue the parent's independent engineering after attaching the collector and
the finite result watch. End the
turn when progress depends on their results. A fresh label is needed when prior
branches reserve the example names. This uses ordinary supervised lifetime; a root
can deliberately choose SwarmOwned for selected leads that should outlive it.
Choose that independently from the task, source and model. Review/repair and later
local frontiers use the same operations below and in [operating.md](operating.md).

## Implement, review, repair in the context that owns the code

A lead implements substantial work and owns its recursive local waves. After
scaffold/fork/integration, bind the exact checked commit as candidate:

```haskell
(reviewer, questions) <- reviewCandidate task OwnerRepairs candidate
let Right reviewReadyLabel = watchLabel "review-ready"
ready <- watch reviewReadyLabel (awaitSettledFork reviewer)
let Right reviewQuestionsLabel = watchLabel "review-questions"
questionReady <- watch reviewQuestionsLabel (awaitProgressAfter questions (ProgressCursor 0))
```

The reviewer returns Produced (Repair latest findings) for defects the lead must
repair. Its attempt settles; the lead's delivery stays open. After local repair and
checks, bind `revised :: Candidate` and reuse the retained reviewer:

```haskell
let Right retryLabel = requestLabel "review-repaired"
(attempt, retryQuestions) <- reviewAgain (forkedActor reviewer) retryLabel (ReviewTask task revised OwnerRepairs)
let Right retryReadyLabel = watchLabel "review-repaired-ready"
retryReady <- watch retryReadyLabel (awaitSettled attempt)
let Right retryQuestionsLabel = watchLabel "review-repaired-questions"
retryQuestionReady <- watch retryQuestionsLabel (awaitProgressAfter retryQuestions (ProgressCursor 0))
```

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
part` returns `(Forked (Outcome Candidate), Progress Attention)`. Watch both. After
that worker returns, reviewCandidate part (RetainedImplementer (forkedActor worker))
latest lets the reviewer request repairs directly. The worker is then available;
queuing repairs behind a lead's pending delivery would deadlock it.

The constituent operations remain available for Haskell composition. For example,
an owner with an existing reply obligation and candidate worker can route that
candidate directly, without a model relay:

```haskell
let destination = sessionReply
forwarding <- route (awaitSettledFork worker) (\settled -> case settled of { ReplyAvailable answer -> reply destination (responseValue answer) >> pure (); ReplyUnavailable failure -> error (T.pack (show failure)) })
```

The destination must have the worker's actual result type. This forwards the
candidate; the recipient still owns independent review and integration. Do not
claim it is a checked delivery. Failed routing retains exceptional attention and
effects for its owner. Callback-local handles are not new resident GHCi bindings:
begin review with bound handles when its questions need your model's judgment.
Never await a new child inside the tool block that is still admitting it.

## Questions stay open until the owning decision arrives

When activation supplies reportProgress, bind `open :: Attention` to cumulative
unresolved questions and `question :: Question` to the concrete finding:

```haskell
let updatedQuestions = raiseQuestion question open
reportProgress updatedQuestions
```

A stable questionKey is local to its plan; source and finding distinguish revisions
of that question. Publish the whole unresolved set so a newly attached collector receives the
current questions. Attached actors receive every subsequent publication. Publish on meaningful changes, not every tool step. Keep this request
pending and continue unrelated useful work. The Sol owner handles the question; this is not a planner notification.
Use followAttention for a single cumulative source, or followAttentionSources for
independently advancing sources and explicit terminal status. Neither needs a
model to poll, concatenate lists and invent another watch label on every update.
The sink chooses what deserves action; publishing status is not an Astra request.
See [coordination.md](coordination.md) for complete routing and consultation examples.

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
the typed decision in its current Task. It can then publish `resolveQuestion
decision open`; an answer to an older version cannot clear a newer finding. Review
with the updated assignment, not the original sessionInput after its contract changed.

## Find the owning helper

Project.Work owns solTask, implement, reviewCandidate, reviewAgain, repair,
designQuestion, consultDesign, withDecision and progress routes. Project.Plan owns
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
