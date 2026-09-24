---
name: exomonad-fork
description: Compose Exomonad implementation children in resident Haskell, choosing inherited or fresh context and collecting typed progress/results. Use when decomposing work with the Project coordination package.
---

Use the resident Haskell tool. The selected package imports Project.Types, Work,
Plan, Routing and Observe. An inherited fork carries conversation, not skill
contents. A child using `withContext (selected taskContext)` reads relevant skills
itself or receives the needed facts in its assignment. Its request-local bindings
come from its own assignment, not the parent's history.

`lunaTaskFrom :: Label -> WorktreeSeed -> Task -> Branch CodingEffects Task result`
builds a branch value on the `luna` alias: the cheap, fast tier, and the default
for bounded implementation and review children, so fork many of them in one
frontier. It selects fresh context from the Task (a Luna cannot reuse a Sol
conversation), so the assignment must carry every fact the child needs.
`solTaskFrom` has the same shape on the `executor` (Sol) alias with inherited
context; use it only for a child that owns design judgment or its own
integration loop. `Task` is a record, not a module; use `taskSource`,
`planPath`, `obligation`, and `acceptedDecisions` directly.

Given your authored `task :: Task` and `source :: WorktreeSeed`, this launches a
fresh Luna Medium implementer returning `Outcome Candidate`, with a progress stream:

```haskell
let workerLabel = [label|implementation|]
let branch = lunaTaskFrom workerLabel source task
(worker, progress) <- unfold (taskGroup task) (childWithProgress @WorkProgress @(Outcome Candidate) branch)
```

Choose `source = currentCheckout` for the executing actor's checkout (root
project checkout or child's bound checkout), `projectHead` for the project source
explicitly, or `atRef (GitRef (renderGitOid commit))` for a committed seed. Use
`solTaskFrom` when a related Sol child should inherit your completed reasoning. Fresh context is useful after bulky reconciliation or for
independent review; descendants within a focused subtree can inherit.

Compose independent children with `((,) <$> child a <*> child b)` inside one
`unfold`. Keep shared-contract and integration work with their parent. Use
`child` when only a final reply is needed; it does not install `reportProgress`.
Retain returned handles. Follow progress/results with the exomonad-coordinate skill.

Messages are for another model: cite the shared plan and send only the assignment
or changed facts it cannot recover. Do not reconstruct the full plan in every Task.
