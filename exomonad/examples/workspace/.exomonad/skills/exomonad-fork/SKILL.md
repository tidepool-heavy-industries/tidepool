---
name: exomonad-fork
description: Compose Exomonad implementation children in resident Haskell, choosing captured or fresh context and collecting typed progress/results. Use when decomposing work with the Project coordination package.
---

Use the resident Haskell tool for scaffold/delegate/integrate; briefly
justify terminal leaves. Give Luna component owners `lunaLead` (Delivery) and
other children `lunaTask` with their real result type. Admit each ready frontier
with `unfoldWork` and retain its original handles. See `RECURSIVE-WORK.md`
for a compiled two-batch example and ready-frontier task construction. The shared package supplies Exomonad.Contrib.Types and Routing;
Project.Work and Project.Observe supply project policy. A captured context carries conversation, not skill
contents. A child using `withContext (selected taskContext)` reads relevant skills
itself or receives the needed facts in its assignment. Its request-local bindings
come from its own assignment, not the parent's history.

Every `unfold`/`child` needs a `ForkGroupPath`. `ForkGroupPath` is a type, not a
term: it has no exported constructor, so `ForkGroupPath "..."` does not
type-check. Reuse `taskGroup work` (or `specialistGroup slot`) when the group
already exists on your `Task`; build a new one with `batch campaign group` (a
fresh two-segment path) or `subgroup group` (nested under your OWN path, which the engine already
knows: pass only the new segment, never your full path) — both take plain
string literals, e.g. `batch "review" "lane-a"`, `subgroup "wave-2"`.

`lunaTaskFrom :: Label -> ForkEffort -> WorktreeSeed -> Task -> Branch CodingEffects Task result`
builds a branch value on the `luna` alias: the cheap, fast tier, and the default
for bounded implementation and review children. Effort (`Low`, `Medium`, `High`)
is chosen at every fork. It selects fresh context from the Task, so the assignment must carry every fact the child
needs, including the contract at each seam it shares with a sibling.
Use the assignment's actual result type for findings or no-change work;
do not manufacture a code Candidate to fit the example below.
`solTaskFrom` has the same shape on the `executor` (Sol) alias with selected Task
context. To reuse a focused scaffold, capture it with `checkpoint` and decorate
the branch with `withContext (fromCheckpoint captured)`.
Use Sol for consequential design judgment. Routine local integration belongs
with the recursive Luna owner. The batch collector routes settlement and question
notices; no watch is needed per child.

## Admit the ready frontier

Given your authored `work :: Task` and `source :: WorktreeSeed`, this admits
one ready obligation returning `Outcome Candidate` and installs its collector.
Include every independent ready obligation in the same list. Use `lunaLeadFrom`
and `deliverySummary` when admitting component owners returning `Delivery`.

```haskell
let implementationBranch = withLifetime ActorOwned $ lunaTaskFrom [label|implementation|] Medium source work :: Branch CodingEffects Task (Outcome Candidate)
Right localBatch <- unfoldWork (taskGroup work) [workChild "implementation" implementationBranch] (notifyWork me (workMessage candidateOutcomeSummary))
let [(_, worker, progress)] = batchMembers localBatch
```

Keep `localBatch` for observations and `finishWorkBatch`; the original `worker`
and `progress` handles remain available for typed follow-up. The coordinate
skill continues from this binding.

## Underlying admission operations

For a custom join or mixed result types, compose the primitives directly. They
implement the same scaffold/delegate/integrate cycle.

`task label objective ownedPaths acceptance source` builds a `Task` with the
group, plan path and empty decisions defaulted; update any field with record
syntax. Fork a wave from `base :: GitOid`: every disjoint obligation plus an
independent review or test child, admitted in one `unfold`:

```haskell
let parserTask = task [label|parser|] "Parse the wire format into Item values" ["src/parse.rs"] "Round-trip tests for every item kind pass" base
let storeTask = task [label|store|] "Persist Items with atomic append" ["src/store.rs"] "Append and replay tests pass" base
let testTask = task [label|contract-tests|] "Write failing tests for the parse/store seam" ["tests/seam.rs"] "Tests compile and name the seam contract" base
let persistent = withLifetime ActorOwned
((parser, parserProgress), (store, storeProgress), (tests, testsProgress)) <- unfold (batch "feature" "wave-1") $ (,,)
  <$> childWithProgress @WorkProgress @(Outcome Candidate) (persistent (lunaTaskFrom [label|parser|] Medium currentCheckout parserTask))
  <*> childWithProgress @WorkProgress @(Outcome Candidate) (persistent (lunaTaskFrom [label|store|] Medium currentCheckout storeTask))
  <*> childWithProgress @WorkProgress @(Outcome Candidate) (persistent (lunaTaskFrom [label|contract-tests|] Low currentCheckout testTask))
Right primitiveQuestions <- followWork [("parser", parser, parserProgress), ("store", store, storeProgress), ("tests", tests, testsProgress)] (notifyWork me workQuestionsMessage)
```

These branches explicitly use `ActorOwned` because their work spans cells.
For an ordinary dependent computation, keep the default invocation lifetime
and await immediate children before returning. End persistent admission promptly; continue independent work or end the turn when
waiting is all that remains. The requests own settlement notices; the collector
surfaces pending questions without duplicate final notices. Read it for question
details and drain `finishWork primitiveQuestions` after all results settle. Before merging a
candidate, refuse paths it does not own:

```haskell
strays <- unownedPaths base (candidateCommit candidate) ["src/parse.rs"]
```

Choose `source = currentCheckout` for the executing actor's checkout (root
project checkout or child's bound checkout), `projectHead` for the project source
explicitly, or `atRef (GitRef (renderGitOid commit))` for a committed seed. Use a retained checkpoint when a related child needs your scaffold reasoning.
Selected context is useful across model tiers and for independent review.

Admission composes one applicative group, not one call per child. Batch the ready, disjoint
obligations; do not admit dependents before their shared contract is available. Keep shared-contract and integration work with the parent. Use
`child` when only a final reply is needed; it does not install `reportProgress`.
Retain returned handles. A record-actor router (`followWork`, see
exomonad-coordinate) consumes settlement itself; wrap those branches in
`withReport Silent`.

Messages are for another model: cite the shared plan and send only the assignment
or changed facts it cannot recover. Do not reconstruct the full plan in every Task.

## Before replying

Return the exact checked candidate and its actual base. Rebase when a required
source correction is missing, overlapping code advanced, or integration conflicts;
disjoint parent changes alone do not require another rebase. Managed worktrees
share local refs, so fetch is needed only for a separate clone. After a rebase,
report the new base and candidate, check cumulative owned paths against that base,
and rerun affected checks. Do not reuse an old ownership verdict.

The parent verifies the exact candidate and cumulative ownership, merges the
child's branch rather than copying files, and checks the resulting integration.
A candidate that no longer applies returns to its owner with the exact dependency
commit and required correction. A delivered source update is not proof of its
incorporation: the owner reports the resulting OID and consumer check.
