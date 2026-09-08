# Initial Astra planner: make this tree effective

You are the Shoal-managed planning root, working with the human in your ordinary
Codex TUI. The external setup conversation only supervises the harness. Both
product designs already exist; focus your substantial reasoning on executing them
well, rather than rewriting them or repeating the product interview.

Start with this wave's README, restart.md and the two short lane maps. Read deeper only for
consequential dependencies or uncertainties. Preserve the full product acceptance.

## Decisions to make before commissioning

- What must each shared context establish before forking? Which reasoning should
  descendants inherit, and which tasks need fresh selected contexts? Separate
  source dependencies from context ancestry. Avoid loading both designs in every
  ancestor; fork before unrelated debugging consumes the useful common prefix.
- What goes in each node's small task packet, linked reference material and upward
  result? Arrange progressive discovery. Keep event handling, routine integration
  and bookkeeping with Sol; reserve your attention for high-leverage decisions.
- Where does Sol have enough shared structure to implement independently? Refine
  the lane maps' Astra slots around actual hard semantic decisions. Give experts
  bounded evidence-rich tasks; do not create standing Astra managers or kill
  useful in-flight work at arbitrary token limits.
- Which scaffold unlocks each ready frontier, and where must results consolidate
  before the next fork? Plan the near frontier precisely and later waves by
  dependencies/outcomes. Preserve local discretion and useful parallelism.

Record the compact decisions in `execution-contract.md` here, with deeper branch
information linked only where needed. Ask the human about consequential direction
changes; the existing goals and two-lane implementation are already authorized.

## Commission and review inside Shoal

Commission one Sol coordinator with Task/Delivery, selected taskContext and:

```haskell
withInstructions (projectPrompt "coordinator")
  (withEffort Medium (componentLead coordinatorLabel task))
```

Use `childWithProgress @Attention @Delivery`; retain its result and progress watches
as shown in the selected run guide. It commissions both Sol leads.
Wait on consolidated Attention, read the exact committed artifacts, then return
concrete corrections and instructions to proceed through `updateRequest` on its pending
response handle. Check presentation; require incorporation before dependent forks.
Do not request a second delivery behind the still-open first one.

After handoff, yield to watches. Sol owns routine execution and review. Stay
available for consequential amendments, human steering and final product intent;
receive selected evidence rather than descendant transcripts.
