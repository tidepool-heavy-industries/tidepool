# The working pattern

For the underlying idea and its rationale, read [composition.md](composition.md):
recursive fork/join inside local integration loops, pairing source and reasoning.
Read the section needed for your current obligation. The supplied activation and
current branch plan are your starting packet. Existing examples in [run.md](run.md)
own the exact review/repair and watch continuations; the shared API guide owns
callable signatures. These are ordinary compositions, not a workflow interpreter.

## Human and initial planner

Use [the planner prompt](../prompts/planner.md) in the human's Astra planning
conversation. In a hosted composition it is selected by
`withInstructions (projectPrompt "planner")`; selecting a model alone does not
select its behavior. Follow that actor's actual input/result and authority.
The resource is on demand, not added to every worker's prompt.

A useful interview resolves the experience through examples. Ask what the user
starts with, chooses/types, sees, can do next, and would consider disappointing.
Use earlier answers. A recommendation and one consequential question usually help
more than a long questionnaire. Establish the finished walkthrough, important
awkward cases, depth/taste, acceptance and allowed intermediate delivery. Record
assumptions explicitly when the human authorizes proceeding without an answer.

The plan connects that endpoint to actual public capabilities, source ownership,
substantial branches and useful partial commits. Any missing capability needs an
owner or a human product decision; documenting a missing feature does not finish
it. The plan spans all needed waves and context generations. Keep current intent
and selected decision rationale concise; older transcripts and superseded plans
remain accessible without becoming every child's orientation.

## Plan understanding and release

Substantive leads first explain their own proposed execution where the assignment
requests a readback. A useful document contains a normal and awkward consumer
walkthrough, actual interfaces/files, local waves and child boundaries, checks,
verified facts, assumptions and questions or suggested changes. Copying the
original plan is not evidence of understanding. Use each other as concrete
consumers: can the proposed UI actually invoke the generator it is being handed?

For a lead with an open Delivery, the readback is progress, not its terminal
response. The following uses ordinary existing values. Bind `source` to the exact
committed readback revision, `summary` to your interpretation, and `evidence`,
`alternatives`, `consumers` to its concrete references/questions. `openQuestions`
is the current cumulative Attention, initially `[] :: Attention` only when none
are open:

```haskell
let details = DesignQuestion (planPath task) source summary evidence alternatives consumers
let question = Question "execution-plan" details
let waiting = raiseQuestion question openQuestions
reportProgress waiting
```

Keep `waiting` as the cumulative set for later publications. The requester watches
that progress, collects the branch documents and takes coupled questions to the
original planner. The review has an explicit recipient, exact input and release
condition. With an external planner, name the pending operator action. With an
available planner actor, use the existing request/result channel with its actual
reply type and watch its result. Never queue to an actor already waiting on you.

Planner steering says what changes, why, affected branches, remaining questions
and which work may now proceed. Owners incorporate it into their current Task and
source; record source/check evidence before resolving the exact question. A
presented update alone proves neither understanding nor incorporation. Complete
this initial checkpoint once, then allow the agreed local waves; repeat planner
review for consequential changed architecture, not every small child or repair.
An operator hold is separate and always requires explicit release.

## Repeated local waves and context choices

Every substantial lead can scaffold, fork a broad useful frontier, integrate
checked results and scaffold again. Children can repeat this recursively. Keep
one owner for each shared mechanism and file seam. Parent delivery stays open
through its own waves. Integrate useful partial source while unrelated children
continue, retaining the remaining component obligation.

Commit a usable scaffold before its consumers fork: real interfaces and minimum
behavior, explicit holes and tests, source ownership and the reasoning for the
boundary. Root-owned UI/API wiring is a deliverable with a recipient and timing;
provide it early enough for dependent lanes to exercise the actual consumer.
A branch awaiting its second scaffold need not stop a sibling's third wave.

For example, inside a lead, bind `source :: Text` to its actual checked integration
hash and `leftTask`/`rightTask` to two current independent assignments, with accepted
decisions already incorporated. Both get that source; their obligations remain
distinct. These bindings prepare branches and launch nothing:

```haskell
let Right wave = forkGroupLabel "wave-2"
let group = subgroup wave
let Right leftLabel = branchLabel "generator"
let Right rightLabel = branchLabel "consumer"
let left = leftTask { taskGroup = group, taskSource = source }
let right = rightTask { taskGroup = group, taskSource = source }
let leftBranch = solTask leftLabel left :: Branch CodingEffects Task (Outcome Candidate)
let rightBranch = solTask rightLabel right :: Branch CodingEffects Task (Outcome Candidate)
```

When their prerequisites and any release condition are met, admit them together:

```haskell
work <- unfold group ((,) <$> childWithProgress @Attention @(Outcome Candidate) leftBranch <*> childWithProgress @Attention @(Outcome Candidate) rightBranch)
let ((leftWork, leftQuestions), (rightWork, rightQuestions)) = work
```

The applicative pair expresses independent admission and preserves the shape of
the returned handles. Register each result/question watch as in run.md and end the
turn. On wake, inspect the ready handle: receiving these handles was not the join.
The join is the owner's subsequent incorporation/checks of the actual candidates;
it can accept a coherent slice while unrelated work remains pending.

`subgroup wave` is relative to the calling actor, not a two-argument path constructor.
Use a fresh local-wave label when earlier branch names remain reserved. Admission
completes when the tool block returns; do not await its new child in that same block.

`inherited` chooses the caller's current completed context boundary. Use it when
shared investigation and accepted decisions are valuable to the child, and fork
before unrelated debugging accumulates. Default solTask/componentLead uses
inherited context and boundHead, as does implement. Original-root callers use
solTaskFrom/componentLeadFrom with projectHead. Select a fresh taskContext
explicitly for unrelated work; use an explicit atRef source for exact committed
inspection. Model selection is independent. Descendants normally remain supervised. A root
may admit SwarmOwned selected leads; that lifetime is not compatible with an
inherited context.

Retain useful specialists after replies and send later exact tasks/decisions.
New requests to a busy actor queue. Existing context does not automatically learn
new parent commits. Saving an arbitrary earlier fork point as checkpointContext
is proposed future work; it is not an operation in this package. Transcript reuse
also does not establish provider cache reuse; report the observed coverage.

## Source, evidence and handoffs

`taskSource`, `candidateCommit` and `decisionSource` are currently Text fields with
specific source meanings. Resolve actual Git hashes; put explanatory prose in
rationale, evidence or the obligation. Prefer existing constructors or a small
record update to reconstructing an entire Task. A corrected AcceptedDecision must
use the integration source the owner checked: withDecision replaces taskSource
with decisionSource, so a stale decision can otherwise move a new task backward.

Distinguish client representation from host contract, absent from observed-null,
and display identity from an authorized handle. An identity accessor may verify
an existing handle without creating one. Use representative public boundary data
or a focused probe before making a consequential absence claim. Keep an unsupported
capability as an explicit question, not a confident negative conclusion.

When emitting code, check meaning as well as text: `let observed = snapshot`
binds an action; `observed <- snapshot` executes it. A helper named the same as
its referenced binding can introduce recursive name capture. Source escaping,
type inference, invocation, and actual retained effects need appropriate checks.
A matched golden string is not proof that the intended operation happened.

Report concise evidence: new fact, exact source, decisive check, open gate and
needed decision or next owner. Preserve original typed results and logs; print
small projections such as workSummary when available. Attribute checks to the
revision actually examined, including inherited baseline failures and unperformed
live gates. Individual workers run focused checks; integration owns the final
combined boundaries. Do not make every small leaf repeat the entire battery.

## Failure, holds and the next wave

Use current receipts and named watches to distinguish failed execution, a pending
reply, an operator hold and a missing external planner action. A historical recap
can mention work already settled; inspect retained state before acting again.
Failed tool units may leave successful prior units and effects intact. Do not
replay an entire launch block because a later observation failed.

For active steering use updateRequest on the owned response, inspect its receipt,
and then seek incorporation evidence. If transport fails, retain that receipt and
report the precise missing delivery once. Use a supported alternate channel only
when authorized; normal Codex TUI steering is the operator interface. Repeated
identical retries or a new queued request cannot substitute for a delivered update.
A substrate owner fixes the mechanism; app workers do not hot-patch their harness.

Close each wave with what works, accepted source, remaining product gates and their
owners, changed assumptions and relevant context references. This is a compact
handoff over existing Git/run evidence. A restart does not create a new product
goal, erase outstanding work or authorize repeating completed assignments. Shared
prompt/module changes remain candidate source until an explicitly authorized
swarm boundary; new local task waves alone do not require a restart.
