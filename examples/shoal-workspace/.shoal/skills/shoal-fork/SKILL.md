---
name: shoal-fork
description: Compose Shoal implementation children in resident Haskell, choosing inherited or fresh context and collecting typed progress/results. Use when decomposing work with the Project coordination package.
---

Use the resident Haskell tool. The selected package imports Project.Types, Work,
Plan, Routing and Observe. Read a relevant skill before related forks so children
inherit useful API knowledge. Their request-local bindings still come from their
own assignment, not the parent's history.

`solTaskFrom :: BranchLabel -> WorktreeSeed -> Task -> Branch CodingEffects Task result`
builds a branch value. `Task` is a record, not a module; use `taskSource`,
`planPath`, `obligation`, and `acceptedDecisions` directly.

Given your authored `task :: Task` and `source :: WorktreeSeed`, this launches a
fresh Sol Medium owner returning `Outcome Candidate`, with a progress stream:

```haskell
let workerLabel = "implementation" :: BranchLabel
let branch = withEffort Medium $ withContext (selected taskContext) $ solTaskFrom workerLabel source task
(worker, progress) <- unfold (taskGroup task) (childWithProgress @WorkProgress @(Outcome Candidate) branch)
```

Choose `source = boundHead` for your current bound checkout, `projectHead` at the
original root, or `atRef (GitRef commit)` for a deliberate committed seed. Omit
`withContext (selected taskContext)` when related children should inherit your
completed reasoning. Fresh context is useful after bulky reconciliation or for
independent review; descendants within a focused subtree can inherit.

Compose independent children with `((,) <$> child a <*> child b)` inside one
`unfold`. Keep shared-contract and integration work with their parent. Use
`child` when only a final reply is needed; it does not install `reportProgress`.
Retain returned handles. Follow progress/results with the shoal-coordinate skill.

Messages are for another model: cite the shared plan and send only the assignment
or changed facts it cannot recover. Do not reconstruct the full plan in every Task.
