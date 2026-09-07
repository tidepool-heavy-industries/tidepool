# Run and improve this application wave

Use a fixed checked Shoal executable. In the application checkout, first run
`shoal check --workspace .` to compile the authored selection without models.
Behavioral recipe checks are being added in the package-curation pass; compilation
alone does not establish their behavior. Starting the paid wave is a subsequent
operator action:

```sh
shoal init --workspace /home/inanna/dev/shoal-repl --session shoal-repl-relations
```

Do not recreate an unfinished or unrelated session. All workers use normal Codex
TUIs; talk directly to the owner, lead or specialist for steering. The original
root's .shoal is authoritative. Candidate files in managed checkouts activate only
after checked incorporation there and an explicit next-swarm selection.

## Start useful owners and retain both result and question handles

In native tools resolve `git rev-parse HEAD`, then bind `baseline :: GitRef` to that
exact app commit. These expressions run in the Sol root's resident environment:

```haskell
let Right campaign = campaignLabel "graph-relations"
let Right leads = forkGroupLabel "leads"
let Right contractLabel = branchLabel "contract"
let Right contractTask = component campaign RelationContract baseline
before <- snapshot
contractWork <- unfold (batch campaign leads) (childWithProgress @Attention @Delivery (withLifetime SwarmOwned (componentLead contractLabel contractTask)))
let (contract, contractQuestions) = contractWork
let Right contractReadyLabel = watchLabel "contract-ready"
contractReady <- watch contractReadyLabel (awaitSettledFork contract)
let Right contractQuestionLabel = watchLabel "contract-questions"
contractQuestionReady <- watch contractQuestionLabel (awaitProgressAfter contractQuestions (ProgressCursor 0))
```

The owner can now end its turn. SwarmOwned selected leads have independent
lifetimes; only a root can admit that lifetime. Their descendants normally remain
supervised. Neither a parent waiting nor a model turn ending settles its request.
On wake, bind `state <- pollWatch contractReady` and inspect `inspectFull state`.
Handle unavailable execution separately from the typed `Blocked reason evidence`
or `Produced (Delivered reviewed head checks)`. No notification proves success.

## Resolve the declared design and transmit its meaning

The contract lead reads its plan and the supplied source. Bind `question ::
DesignQuestion` with the concrete uncertainty, evidence, alternatives and consumers:

```haskell
let Right designCampaign = campaignLabel "graph-contract"
let Right slot = relationDesign designCampaign
(expert, designReady) <- consultDesign slot question
```

End the turn while retaining the component obligation. On wake inspect the answer.
A Decision is supported reasoning; AmendPlan is a proposed commit; NeedEvidence
identifies a missing fact. Accept within-plan choices locally and take consequential
scope/acceptance changes to the human. Incorporate/check shared semantics and
record them in the committed contract before dependent branches start.

Bind `resolved :: Question`, `head :: Text`, `summary :: Text`, and `checks ::
[Text]` to the accepted question, actual incorporated revision, decision and evidence:

```haskell
let decision = AcceptedDecision resolved head summary checks
let task = withDecision decision sessionInput
```

Use task for implementation and review. It carries the rationale into a fresh
context, as well as the source that now embodies it. withDecision is a pure packet
transformation; it does not run Git or establish acceptance. Never construct a
receipt from a proposed commit you have not incorporated.

## Implement, review, repair in the context that owns the code

A lead normally implements itself. Bind its checked commit as candidate:

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

Delegate implementation only when it creates useful independent work. `implement
part` returns `(Forked (Outcome Candidate), Progress Attention)`. Watch both. After
that worker returns, reviewCandidate part (RetainedImplementer (forkedActor worker))
latest lets the reviewer request repairs directly. The worker is then available;
queuing repairs behind a lead's pending delivery would deadlock it.

The constituent operations remain available for Haskell composition. For a real
routed delegation, reviewFrom takes a consumer of explicit unavailable/blocked
results or the new review/result-progress handles. Bind/register those handles in
the consumer; do not discard the next obligation. Successful routing needs no
model relay. Watch/route callbacks run after their dependencies become ready;
never wait for a new child inside the tool block that is still admitting it.

## Questions stay open until the owning decision arrives

When activation supplies reportProgress, bind `open :: Attention` to cumulative
unresolved questions and `question :: Question` to the concrete finding:

```haskell
let updatedQuestions = raiseQuestion question open
reportProgress updatedQuestions
```

A stable questionKey is local to its plan; source and finding distinguish revisions
of that question. Publish the whole unresolved set so coalescing loses no unanswered
question. Publish on meaningful changes, not every tool step. Keep this request
pending and continue unrelated useful work. The owner inspects the question watch
and rearms `awaitProgressAfter questions cursor` at the returned ProgressCursor.
The ordinary watch alerts the owning model; followAttention can instead connect
that source directly to a Haskell consumer when a relay would add no judgment.
Its sink receives changes to one cumulative source. Combining sources requires
preserving their union, not overwriting all attention with whichever source changed.

After an owning decision, bind `decision :: AcceptedDecision` as above. On the
request owner's retained response, for example the initial review:

```haskell
delivery <- updateRequest (forkedResponse reviewer) (decisionContext decision)
```

Handle Left explicitly; on Right bind and poll the RequestUpdate handle.
UpdatePresented means the steering was presented, not that code was incorporated.
UpdateUnconfirmed/UpdateNotPresented/UpdateTooLate require examining that receipt
and current work before intervention; do not silently enqueue a replacement request.
For the retained attempt use attempt itself instead of forkedResponse reviewer.
Forward through each response owner where an intermediate lead owns the review.

The recipient reads that supported steering, verifies incorporation and records
the typed decision in its current Task. It can then publish `resolveQuestion
decision open`; an answer to an older version cannot clear a newer finding. Review
with the updated assignment, not the original sessionInput after its contract changed.

## Incorporate the contract, then work on independent consumers

The owner incorporates the reviewed contract and checks the resulting revision.
Bind `acceptedContract :: GitRef` there and the relevant checked decision(s) from
acceptedAssignment. Include them in both new Tasks using withDecision after updating
decisionSource to the actual incorporated baseline. Read the committed contract too.

```haskell
let Right projectionBase = component campaign RelationProjection acceptedContract
let Right controlsBase = component campaign RelationControls acceptedContract
let projectionTask = withDecision contractDecision projectionBase
let controlsTask = withDecision contractDecision controlsBase
let Right products = forkGroupLabel "product"
let Right projectionLabel = branchLabel "projection"
let Right controlsLabel = branchLabel "controls"
(projectionWork, controlsWork) <- unfold (batch campaign products) ((,) <$> childWithProgress @Attention @Delivery (withLifetime SwarmOwned (componentLead projectionLabel projectionTask)) <*> childWithProgress @Attention @Delivery (withLifetime SwarmOwned (componentLead controlsLabel controlsTask)))
```

Register independent result and question watches as for contractWork; either
component can progress or deliver while the other awaits a decision. Integrate
coherent deliveries independently. The owner runs combined all-target checks,
graph/control regressions, formatting and isolated terminal proof before claiming
the product gate closed. Preserve composer state and prove no graph-triggered POST.
Inspect listRoutes/pollRoute and retained effects after coordination failure before
retrying; launched work and normal TUIs may still be useful.

## Requested RSI from selected evidence

```haskell
projectionEvidence <- observeWork projectionTask projectionWork
controlsEvidence <- observeWork controlsTask controlsWork
later <- snapshot
inspectFull (workSummary projectionEvidence)
inspectFull (usageDelta before later)
```

Bind source to the exact integrated app commit, question to the human's improvement
request and evidence to precise friction/artifact references, all as Text/[Text].

```haskell
let packet = RsiInput source question [projectionEvidence, controlsEvidence] before later evidence
let Right improvements = forkGroupLabel "requested-improvement"
let Right improvementLabel = branchLabel "workspace-style"
improvement <- unfold (batch campaign improvements) (child (withLifetime SwarmOwned (rsiBranch improvementLabel (atRef (GitRef source)) packet)))
let Right improvementReadyLabel = watchLabel "improvement-ready"
improvementReady <- watch improvementReadyLabel (awaitSettledFork improvement)
```

Snapshots preserve unknown usage coverage, exact identities and provider staleness.
Zero queued requests alone does not prove a useful worker is available. Requested
model counts are not billing totals or proof of work quality. Expand observations
only for the decision at hand. shareObservation can grant this ordinary RSI worker
read-only scope over a selected component without transferring stop authority.

RSI returns Outcome Candidate for a checked next-wave customization. Incorporate
it in the original root .shoal and activate explicitly after unfinished work ends
or is deliberately handed off. Confirm the next selected identity/preview consumes
the edit while the old wave remains frozen. Live usability and savings are evidence
from the subsequent application run, not from deterministic recipe checks.
