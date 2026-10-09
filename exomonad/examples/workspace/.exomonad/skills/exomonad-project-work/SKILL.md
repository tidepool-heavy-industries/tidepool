---
name: exomonad-project-work
description: "Plan and deliver Git-backed project changes when the task needs scoped ownership, review, and checked integration. Load when changing or planning a repository; the workflow is optional authored composition."
---

Use this skill for Git-backed project delivery. A useful sequence is to establish
the shared contract and source revision, assign work whose dependencies are
ready, review exact candidate commits, repair findings, and verify the integrated
revision. Choose the number and shape of contributors from the work itself. A
single bounded change may have one owner; a broad change may have independent
implementation, review, or investigation work. No model tier, actor role, group
name, or batch abstraction is required.

General exploration, semantic pipelines, and actor protocols use their own
compositions. This workflow is a project-specific method a notebook author may
choose. Use typed agent requests and `Await` values from the shared API. Keep
Haskell definitions and record actors task-shaped; add a reusable Project helper
only when an authored consumer needs it.

## Establish the shared boundary

Write down the intended result, relevant source revision, shared types and
semantics, production consumer, owned paths, local acceptance, and integration
owner. Resolve decisions that would make independent work incompatible. Keep
permitted holes explicit and distinguish compiled code from implemented
behavior. Assign ready work with the contract and evidence its owner needs, then
continue independent work while requests run.

For code changes, each implementation owner compiles affected targets and runs
its focused acceptance through the repository's supported commands. Independent
review can proceed alongside those checks. Coordinate concrete resource or shared
output conflicts; report a blocked check explicitly rather than routing routine
validation through a central build owner.

A request should carry its objective, source identity, owned paths, dependencies,
acceptance, relevant evidence, and escalation conditions. Include the failure
mechanism and useful method cues; leave routine implementation choices to its
owner. A captured context is a snapshot. Later decisions and corrections need
explicit delivery, and delivery alone is not evidence of incorporation.

## Review and repair

Review substantial implementation at its exact candidate commit. Check the
production consumer, the accepted contract, ownership across the cumulative diff,
and relevant failure and cleanup paths. A source change, successful compilation,
and executed behavior are separate evidence. A selected test that runs zero cases
did not pass. Route actionable findings to an implementer, or make a bounded
repair when that is your ownership.

When source corrections or overlapping work invalidate a candidate, rebase and
recheck the cumulative change against the new base. Report the exact candidate,
actual base, checks and remaining gaps. A component result does not establish
combined acceptance until the integrated revision is checked.

## Deliver

Integrate the reviewed source through Git so its commit relationship remains
inspectable. Verify the resulting revision and report incorporated candidates,
executed checks, consequential assumptions, and unverified behavior. Preserve
user work and retain source and evidence. Remove completed handoffs from standing
documentation; Git preserves history.
