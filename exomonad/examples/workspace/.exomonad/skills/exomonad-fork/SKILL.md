---
name: exomonad-fork
description: Compose Exomonad implementation children in resident Haskell, choosing inherited or fresh context and collecting typed progress/results. Use when decomposing work with the Project coordination package.
---

Use the resident Haskell tool. The selected package imports Project.Types, Work,
Plan, Routing and Observe. An inherited fork carries conversation, not skill
contents. A child using `withContext (selected taskContext)` reads relevant skills
itself or receives the needed facts in its assignment. Its request-local bindings
come from its own assignment, not the parent's history.

Every `unfold`/`child` needs a `ForkGroupPath`. `ForkGroupPath` is a type, not a
term: it has no exported constructor, so `ForkGroupPath "..."` does not
type-check. Reuse `taskGroup work` (or `specialistGroup slot`) when the group
already exists on your `Task`; build a new one with `batch campaign group` (a
fresh two-segment path) or `subgroup group` (nested under the enclosing fork's
own path) — both take plain string literals, e.g. `batch "review" "lane-a"`.

`lunaTaskFrom :: Label -> ForkEffort -> WorktreeSeed -> Task -> Branch CodingEffects Task result`
builds a branch value on the `luna` alias: the cheap, fast tier, and the default
for bounded implementation and review children. Effort (`Low`, `Medium`, `High`)
is chosen at every fork. It selects fresh context from the Task (a Luna cannot
reuse a Sol conversation), so the assignment must carry every fact the child
needs, including the contract at each seam it shares with a sibling.
`solTaskFrom` has the same shape on the `executor` (Sol) alias with inherited
context; use it only for a child that owns design judgment or its own
integration loop. The child's settlement notice wakes you with its reply; no
watch is needed per child.

Given your authored `work :: Task` and `source :: WorktreeSeed`, this launches
one fresh Luna implementer returning `Outcome Candidate`, with a progress stream:

```haskell
let branch = lunaTaskFrom [label|implementation|] Medium source work
(worker, progress) <- unfold (taskGroup work) (childWithProgress @WorkProgress @(Outcome Candidate) branch)
```

`task label objective ownedPaths acceptance source` builds a `Task` with the
group, plan path and empty decisions defaulted; update any field with record
syntax. Fork a wave from `base :: GitOid`: every disjoint obligation plus an
independent review or test child, admitted in one `unfold`:

```haskell
let parserTask = task [label|parser|] "Parse the wire format into Item values" ["src/parse.rs"] "Round-trip tests for every item kind pass" base
let storeTask = task [label|store|] "Persist Items with atomic append" ["src/store.rs"] "Append and replay tests pass" base
let testTask = task [label|contract-tests|] "Write failing tests for the parse/store seam" ["tests/seam.rs"] "Tests compile and name the seam contract" base
((parser, parserProgress), (store, storeProgress), (tests, testsProgress)) <- unfold (batch "feature" "wave-1") $ (,,)
  <$> childWithProgress @WorkProgress @(Outcome Candidate) (lunaTaskFrom [label|parser|] Medium currentCheckout parserTask)
  <*> childWithProgress @WorkProgress @(Outcome Candidate) (lunaTaskFrom [label|store|] Medium currentCheckout storeTask)
  <*> childWithProgress @WorkProgress @(Outcome Candidate) (lunaTaskFrom [label|contract-tests|] Low currentCheckout testTask)
```

End the turn after admission; each child's notice wakes you. Before merging a
candidate, refuse paths it does not own:

```haskell
strays <- unownedPaths base (candidateCommit candidate) ["src/parse.rs"]
```

Choose `source = currentCheckout` for the executing actor's checkout (root
project checkout or child's bound checkout), `projectHead` for the project source
explicitly, or `atRef (GitRef (renderGitOid commit))` for a committed seed. Use
`solTaskFrom` when a related Sol child should inherit your completed reasoning. Fresh context is useful after bulky reconciliation or for
independent review; descendants within a focused subtree can inherit.

Admission costs one cell per `unfold`, not per child, so admit the whole wave
at once. Keep shared-contract and integration work with the parent. Use
`child` when only a final reply is needed; it does not install `reportProgress`.
Retain returned handles. A record-actor router (`followWork`, see
exomonad-coordinate) consumes settlement itself; wrap those branches in
`withReport Silent`.

Messages are for another model: cite the shared plan and send only the assignment
or changed facts it cannot recover. Do not reconstruct the full plan in every Task.

## Before replying

A child rebases onto the parent's current integration head before replying,
and the parent merges the child's branch rather than copying files. Every
managed worktree is a linked worktree of the same repository (`git worktree
add` off the shared `.git`), so the parent's branch is already an ordinary
local ref in the child's checkout — no fetch is needed unless the child is
working from a separate clone. In the child's own checkout, through the `bash`
tool: `git rebase <parent branch>` (named in the assignment; otherwise derive
it from the ancestor segments of your own actor path,
`exomonad/<ancestor path>/branches/<leaf>`), re-run the check command your
assignment named, and only then `respond`. On the parent side, once
`unownedPaths` finds no strays, integrate the rebased commit — `git merge
--no-ff <child branch>` in the held integration worktree, or `publish` on a
`Project.Merge` actor bound to it (`PublishRequest` naming the child's
commit; it merges, runs the project check, and only advances the branch on a
green result). A stale candidate — the parent's head moved again after the
rebase — goes back to the child with `sendMessage` asking it to rebase again
before resubmitting.
