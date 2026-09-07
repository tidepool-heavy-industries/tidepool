# Shared project language

- **Actor key**: exact `(id, incarnation)`. A later incarnation is a different
  actor. Labels are display text, not keys or authorization.
- **Creator**: the actor which admitted this actor. Creation provenance survives
  an independent worker outliving its creator. It is not lifetime supervision.
- **Supervisor**: the owner of supervised lifetime. An independent root has no
  supervisor. Never synthesize one from creator or context ancestry.
- **Context parent**: the actor whose completed context boundary was inherited.
  A selected fresh context has none, even when another actor created it.
- **Relation view**: a client-side projection choosing which observed relationship
  supplies display edges. It grants no authority and does not mutate raw data.
- **Presentation root**: a root of the currently selected display forest. A
  missing parent or cycle break does not turn the actor into a runtime root.
- **Candidate**: a committed source revision with checks and remaining gates.
  **ReviewedCandidate** retains exact reviewed head and review evidence.
- **Delivered**: a reviewed candidate and the exact resulting commit checked by
  its owner. That head may differ after integration. The application owner still
  incorporates and checks the result in the shared app branch.
- **Task**: source, obligation, rationale, owning scope, acceptance and relevant
  accepted decisions supplied to a fresh context. A plan path alone is insufficient.
- **Attention**: the cumulative unresolved Question set published by an active
  request. An AcceptedDecision records the exact answered question, checked source,
  reasoning and evidence; it grants no authority and cannot erase a newer question.
- **Outcome**: Produced value or Blocked reason evidence. This product conclusion
  is separate from a Settlement reporting whether execution supplied any reply.
- **PlanAmendment**: proposed plan-source commit, exact base, affected paths and
  obligations, rationale and evidence. **Incorporated** is a separate recipient
  receipt with resulting head and checks. Receiving a proposal is not adoption.

The Haskell task/result types encode these coordination distinctions. Rust owns
runtime authority and actor/request identity. Compact messages should keep the
candidate, evidence, uncertainty, current owner and next useful action. Do not
compress away spaces, units, negation or the difference between a claim and check.
