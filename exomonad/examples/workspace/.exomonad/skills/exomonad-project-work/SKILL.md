---
name: exomonad-project-work
description: "Default Git project-work mode: recursively scaffold and delegate implementation, review exact candidates, repair, integrate and verify. Load when planning or changing a Git project; owns the Sol/Luna hierarchy and delivery policy."
---

Use this mode for Git-backed project implementation and delivery. The cycle is
**scaffold → admit ready implementation → review and repair exact candidates →
integrate and verify → repeat**. The Sol root owns cross-component choices;
Luna component owners repeat the cycle through subcomponents to microtask leaves.
Role authority and the current assignment determine what each actor can do.

Express this workflow in ordinary Haskell. The supplied Project helpers implement
useful parts; your own control language and record actors can automate admission,
joins, review routing and follow-up. Preserve the source, ownership and acceptance
relationships below as you compose those machines. General notebook experiments,
semantic pipelines and actor protocols use their own task-shaped compositions;
the Git project assignment selects this workflow.

## Scaffold the next useful boundary

Define the outcome, shared types and semantics, production consumer, source
revision, owned paths, local acceptance and integration owner. Commit the minimum
usable shared types and consumer wiring so independent obligations can start.
Name permitted holes and distinguish compilation from implemented behavior.
Keep shared decisions and integration with the owner; delegate implementation
as well as tests.

Execution owners scaffold and recursively delegate. Before substantial direct
implementation, briefly explain a terminal leaf: one bounded change with no
useful independent implementation frontier. A small leaf needs no approval.
Aim for at least three Luna implementation levels on average. Count useful
implementation depth; reviewers and idle forwarding nodes do not add a level.
Each owner must create real parallel work or remove a shared dependency; reassess
a shallow split before doing a whole component alone. Review-only, integration
and runtime leaf roles retain their declared limits.

Admit the ready obligations together. Do useful local work while children run,
integrate coherent results, then construct the next batch from what is now known.
A branch whose assignment needs another result belongs in the later stage that
has that result. Start the next ready batch without waiting for unrelated siblings.

## Compose assignments and admission

`Project.Work` supplies the Git task vocabulary. Use `lunaLead` for component
`Delivery`, `lunaTask` for other typed results, and `unfoldWork` for admission
plus event collection. Use the real result type for findings or no-change work.
At the root, build Tasks with the project's constructors; `sessionInput` and
`respond` belong to an active request and are absent there. Load
[exomonad-fork](../exomonad-fork/SKILL.md) for these branch constructors and
[exomonad-coordinate](../exomonad-coordinate/SKILL.md) for batch collectors.

Use `currentCheckout` for children seeded from the executing owner's checkout,
`projectHead` for the project source, or an explicit committed ref for an exact
seed. Use selected context across model tiers and for independent review; focused
Luna descendants can inherit useful scaffold reasoning from a checkpoint.
Context inheritance is a snapshot. Later decisions and source corrections need
explicit delivery and checked incorporation.

Give each peer the objective, shared contract, production consumer, owned paths,
source revision, dependencies, local acceptance, focused checks and escalation
conditions. Reference shared decisions and evidence. Supply method cues and the
failure mechanism that matters, then leave ordinary implementation choices to
the recipient. The child's local gate supports the parent's stronger combined
acceptance.

The collector owns its subscribed question and settlement notices. Read
`batchRouter` on actionable wakes; use the original response/progress handles for
follow-up. A pending question leaves the assignment open. Answer it before
resuming a joint settlement wait. `Blocked` means terminal inability, not a
question or findings report. Contract corrections name the superseded decision,
exact source, affected consumers and required check; acknowledgment is followed
by the recipient's resulting OID and check evidence.

## Review and repair candidates

Substantive code candidates receive independent review; findings-only work does
not. Reviewers never fork reviewers. Leaf review checks its change; component
review checks joins and combined acceptance using the accepted leaf evidence.
Load [exomonad-review](../exomonad-review/SKILL.md) for exact-source review and
the supplied bounded repair flow.

Seed the reviewer at the exact candidate commit so its checks run that candidate.
Inspect production consumers and relevant failure or cleanup paths. Retain an
implementer for repairs on the same files; use a fresh child for independent
review, test design or a disjoint change. An implementer returns a revised
candidate or a decision need through the pending request; queuing it back to a
reviewer waiting on that reply would create a dependency cycle.

Rebase when a required source correction is missing, overlapping code advanced,
or integration conflicts. Disjoint parent changes alone do not require a rebase.
After a rebase, report the new base and candidate, recheck cumulative owned paths
against that base, and rerun affected checks. A partial component review names
the integration gates that remain.

## Integrate and deliver

Verify the exact candidate and cumulative ownership. Merge the child's commit
through `tryMerge` or Git to retain its source relationship. A candidate that no
longer applies goes back to its owner with the dependency commit and required
correction. Check the resulting integration revision: individually passing
components can disagree at a shared boundary.

Return exact candidates and actual bases, incorporated revisions, executed check
counts, unverified behavior, consequential assumptions and enough evidence to
reproduce a finding or decide the next action. Publication, review, integration
and recipient incorporation are distinct evidence. Use the smallest meaningful
checks; broaden for changed risk or project requirements.

Include brief kaizen in delivery; send it to the owner before the terminal typed
reply. Keep specialists and collectors through useful repairs. After review,
integration and checks, finish collectors and load
[exomonad-cleanup](../exomonad-cleanup/SKILL.md) to retire completed fork groups.
Pending members block whole-group cleanup; retain workers for named work.
Settlement and collector closure do not release actors. Read cleanup receipts
and later host release notices.

For the compiled batch and review compositions, read
[RECURSIVE-WORK.md](../../RECURSIVE-WORK.md). Use
[exomonad-unfold](../exomonad-unfold/SKILL.md) for primitive admission and joins,
and [exomonad-define-actors](../exomonad-define-actors/SKILL.md) to encode the
workflow's transitions in a stateful interpreter.
