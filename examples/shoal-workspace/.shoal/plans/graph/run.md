# Optional graph feature walkthrough

This is the shoal-repl example, not the current assignment. Read only if its
concrete allocation or requested RSI recipe helps. The [general run guide](../run.md)
owns watches, review/repair and steering. Between the steps below, use its result
handling and perform the owning integration/checks; returned handles are not joins.

## Worked graph allocation: result and question handles

Choose an unused campaign label for each new wave; Git branches from earlier
waves are retained. The label below is a first-run example, not a name to replay
on every restart. Use subgroup for work scoped under an existing actor.

In native tools resolve `git rev-parse HEAD`, then bind `baseline :: GitRef` to that
exact app commit. Bind `onQuestions` to the Sol owner's handling/steering policy
from [coordination.md](../coordination.md#independent-progress-without-relay-turns).
These expressions run in the Sol root's resident environment:

```haskell
let Right campaign = campaignLabel "graph-relations"
let Right leads = forkGroupLabel "leads"
let Right contractLabel = branchLabel "contract"
let Right contractTask = component campaign RelationContract baseline
before <- snapshot
contractWork <- unfold (batch campaign leads) (childWithProgress @Attention @Delivery (withLifetime SwarmOwned (withContext (selected taskContext) (componentLeadFrom contractLabel projectHead contractTask))))
let (contract, contractQuestions) = contractWork
let Right contractReadyLabel = watchLabel "contract-ready"
contractReady <- watch contractReadyLabel (awaitSettledFork contract)
contractAttention <- followAttention contractQuestions onQuestions
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
(projectionWork, controlsWork) <- unfold (batch campaign products) ((,) <$> childWithProgress @Attention @Delivery (componentLeadFrom projectionLabel projectHead projectionTask) <*> childWithProgress @Attention @Delivery (componentLeadFrom controlsLabel projectHead controlsTask))
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
