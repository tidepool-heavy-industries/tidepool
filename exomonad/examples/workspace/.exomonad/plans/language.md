# Shared project language

- **Obligation**: an owned outcome with scope and acceptance. A substantial node
  can own it through several local waves; one completed plan or wave is not delivery.
- **Fork point**: the chosen source revision and useful completed reasoning
  boundary, or a selected focused context. Exact source and accepted decision
  changes remain explicit; neither a context nor a worktree updates retroactively.
- **Ready frontier**: independent obligations whose shared prerequisites are met.
  Fork broadly there; a shared unresolved decision gates only its dependent work.
- **Join / fold**: the owner's incorporation and checks of the needed results,
  reconciliation of decisions, and retention of remaining work and evidence.
  Source is integrated; transcripts are not merged. There is no separate join API.
- **Local wave**: one node's scaffold/fork/integrate cycle over exact source and
  decisions. Siblings can be on different waves. Its continuation carries the
  remaining obligation, updated source/decisions and useful pending handles.
- **Readback**: an owner's concrete execution interpretation, objections and
  questions for planner review. It is progress while implementation Delivery is
  pending; it does not itself finish the feature.
- **Planner release**: the owning decision allowing identified work to proceed
  after corrections. An external planner remains an explicit operator action.
  An operator hold requires explicit release; watch notices do not provide it.
- **Actor key**: exact `(id, incarnation)`. A later incarnation is a different
  actor. Labels are display text, not keys or authorization.
- **Creator**: the actor which admitted this actor. Creation provenance survives
  an independent worker outliving its creator. It is not lifetime supervision.
- **Supervisor**: the owner of supervised lifetime. An independent root has no
  supervisor. Never synthesize one from creator or context ancestry.
- **Context parent**: the actor whose completed context boundary was inherited.
  A selected fresh context has none, even when another actor created it.
- **Candidate**: a committed source revision with checks and remaining gates.
  **ReviewedCandidate** retains exact reviewed head and review evidence.
- **Delivered**: a reviewed candidate and the exact resulting commit checked by
  its owner. That head may differ after integration. The application owner still
  incorporates and checks the result in the shared app branch.
- **Task**: source, obligation, rationale, owning scope, acceptance and relevant
  accepted decisions supplied to a fresh context. A plan path alone is insufficient.
- **Attention**: the cumulative unresolved Question set published by an active
  request, retained by Sol execution owners rather than forwarded to the planner.
  Hard questions use explicit consultDesign requests. An AcceptedDecision records the exact answered question, checked source,
  reasoning and evidence; it grants no authority and cannot erase a newer question.
- **Outcome**: Produced value or Blocked reason evidence. This product conclusion
  is separate from a Settlement reporting whether execution supplied any reply.
- **PlanAmendment**: proposed plan-source commit, exact base, affected paths and
  obligations, rationale and evidence. **Incorporated** is a separate recipient
  receipt with resulting head and checks. Receiving a proposal is not adoption.

The Haskell task/result types encode these coordination distinctions. Rust owns
runtime authority and actor/request identity. Intra-swarm messages use the shared compact-communication policy: no mandatory
format; include only what the recipient needs for correct next action. Preserve executable syntax, units, negation, authority and claim/check distinctions.
