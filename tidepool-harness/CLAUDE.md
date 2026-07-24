# tidepool-harness — typed-yield session tree (R0 scaffold)

New frontend over the eval substrate — NOT a retrofit of tidepool-repl
(which keeps its parked-thread mechanism unchanged). Plan + segment specs:
`plans/harness-r0/`; cross-segment contracts:
`plans/harness-r0/00-scaffold/contracts.md`.

Current contents are the contract vocabulary only:
- `tree` — NodeId/NodeState/HoleId/SiteId, forcing badges, generic `Slot<M>`
- `log` — E4 event-log wire schema (header pins prelude+extract; `Effect`
  events carry req AND resp — replay is effect-response substitution)
- `provider` — `ModelProvider` trait (calling-model turns; not the Llm effect)

Landing here per segment: 20 resident-session registry (instantiates
`Slot<M>`), 30 log writer/replayer/forcing/scheduler, 60 provider impls.

Rules inherited from the plan: forcing events are the only work-begins
mechanism (consent integrity audits to literal zero); teasers are
harness-generated only; suspended machines follow the stowed-XOR-running
discipline — all machine access goes through the registry.
