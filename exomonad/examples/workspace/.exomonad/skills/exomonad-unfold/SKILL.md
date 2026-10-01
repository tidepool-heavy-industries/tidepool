---
name: exomonad-unfold
description: Admit several Exomonad children in one applicative unfold, join their settlements in one watch, and read a child's committed work from the parent's own Git view. Load when decomposing work across children and inspecting what they produced.
---

The execution workflow is recursive scaffold/delegate/integrate, using
`lunaLead`/`lunaTask` and `unfoldWork` for each ready frontier. This skill explains
the underlying admission and join primitives for custom Haskell compositions.
See `RECURSIVE-WORK.md` for the canonical procedure.

`unfold` publishes a typed applicative group immediately from captured or selected
context. `unfoldDeferred` publishes after the enclosing call's real result;
its branches require explicit persistent lifetime. Keep shared contracts and
integration with the parent.

`batch campaign group` builds the `ForkGroupPath`; it has no exported constructor.
Use `subgroup group` under your own existing path, passing only the new segment.

```haskell
Right captured <- checkpoint "corpus-scaffold"
let domainPlan = "Add the shared item type and its tests." :: Text
let consumerPlan = "Update the readers of that type." :: Text
let fromScaffold = withContext (fromCheckpoint captured)
workers <- unfold (batch "corpus" "fanout") $
  (,) <$> child @(Outcome Candidate) (fromScaffold (coding currentCheckout (assignment [label|domain|] domainPlan)))
      <*> child @(Outcome Candidate) (fromScaffold (withEffort Medium (coding currentCheckout (assignment [label|consumer-tests|] consumerPlan))))
joined <- waitFor ((,) <$> awaitSettled (fst workers) <*> awaitSettled (snd workers))
```

`waitFor` suspends this continuation and returns `Either WatchFailure result`.
Default `InvocationOwned` children settle within this invocation or are cancelled
on exit with cleanup retained. For work spanning model turns, decorate each
branch with `withLifetime ActorOwned`, retain the original handles, and register
named watches or a persistent collector. Returning handles does not extend work.

Immediate admission requires `fromCheckpoint` or `selected`, refusing unresolved
`inherited` context before allocation. A checkpoint retains the exact Haskell
scope and provider prefix before the current call; dependencies from earlier
pending calls remain real dependencies. `releaseCheckpoint` prevents future
use while admitted children keep their independent leases.

Use explicit `ActorOwned` with `unfoldDeferred` for the enclosing call's final
scope and real result. Return promptly from that invocation; never await a
child whose publication needs its return. The deferred result is never fabricated.

`spawnWatched` admits one child and registers its settlement watch. For several
children compose their original typed `Await` values: `awaitSettled` keeps failure
in the result; `awaitResponse` makes an unavailable dependency a watch failure.
`awaitAnySettled` selects the first settlements while preserving input order.
Borrowed responses support observation; updates, cancellation and release remain
with the owner. Record-actor handlers remain serialized while suspended.

## Observe a child's submission

A `Response` carries an immutable launch receipt on its launch request, and
that receipt's `cwd` names the child's own checkout. **It is not a path the
parent can read while the child lives.** The child owns that worktree; its
contents are mid-edit, and a path that resolves in your shell is a different
directory or a stale one. Nothing about holding the handle grants file access.

Use the identity in the receipt — the branch and the commit — resolved against
the repository from your own view. Typed worktree observations are also readable
through an inherited handle while its checkout remains available:

```haskell
let launch = launchedWorktree <$> responseAdmission worker
let seedOid = maybe "" (renderGitOid . sourceHead) launch
let onBranch = maybe "" (\receipt -> case branch receipt of BranchName b -> b) launch
(seedOid, onBranch)
```

After settlement the typed result carries the submitted commit and a typed
observation of it. Read those instead of the filesystem:

```haskell
state <- pollResponse worker
let evidence = case state of { ResponseReady result -> Just (responseWorktree result); _ -> Nothing }
case evidence of
  Just (WorktreeObserved _ submitted observation) -> (renderGitOid submitted, committedPaths observation)
  _ -> ("no submission observed yet" :: Text, [])
```

`committedPaths` answers "what did it change" without opening a single file.
With the OID in hand, ordinary Git from the parent's own working directory
reads the content — `git show <oid> -- <path>` for one file's change,
`git show --stat <oid>` for the shape of the whole submission:

```haskell
let oid = "abc123" :: Text
statOut <- Cmd.stdout <$> Cmd.run (Cmd.withArguments [oid] [bash|git show --stat --oneline "$1"|])
either (const "submission not visible from here") (T.take 2000) statOut
```

A commit the parent cannot resolve means the child has not checkpointed it yet,
not that the work is missing; poll the response rather than guessing at paths.
`observeSubmission`, `worktreeBranch` and `worktreeHead` are the typed
equivalents when you hold a worktree handle. After checking the observed
candidate, merge its exact submitted head into your managed integration tree:

```haskell
mergeObserved :: Member WorktreeIntegration effects
  => WorktreeHandle -> WorktreeEvidence
  -> Eff effects (Maybe (Either WorktreeError MergeOutcome))
mergeObserved integrationTree evidence = case evidence of
  Just (WorktreeObserved _ _ observation) -> Just <$> tryMerge MergeRequest
    { mergeSourceHead = headOid (submittedHead observation)
    , mergeSourceWorktree = observedWorktreeId observation
    , mergeSourceBranch = Nothing
    , mergeTargetWorktree = worktreeId integrationTree
    , mergeMessage = "Integrate reviewed submission"
    , mergeAdvance = Nothing
    }
  _ -> pure Nothing
```

The `Nothing` branch means there is no submitted candidate to merge.

## Artifacts travel in the reply

A child's product reaches you as its typed settlement value — candidates,
findings, check evidence, unresolved decisions — plus the commit that backs it.
Do not ask a child to copy files into a shared directory, and do not go looking
for them: define the result type so the artifact is a value, and let the Git
identity carry anything too large to be one. A typed reply is evidence of
execution, not of integration; verify the submitted commit before merging.

Require committed candidates in implementation assignments: commit to the child's
own branch before replying and include the exact commit in the result. A passing
check over uncommitted files does not establish a submitted candidate.

## Source admission and follow-up

`currentCheckout` selects the executing actor's checkout: root project source or
child's bound worktree. `projectHead` selects the project source explicitly.
Admission checkpoints eligible edits on the source branch, including
root main, without hooks or checks. Runtime `.exomonad/`, configured exclusions, and
recognized caches are excluded. Git checkpoint failure preserves working files
and refuses the fork. A busy native source uses existing committed HEAD and
reports omitted edits. An optional overlay capture that is busy or unavailable
uses checkpointed HEAD and reports its omission.
Use `atRef` for an explicit committed baseline; inspect the admission receipt.

Use `request` for new work on a retained worker, `updateRequest` for clarification
of its active assignment, and `sendMessage` for information. Inspect the accepted
update with `pollRequestUpdate`; admission, presentation, and checked incorporation
remain separate evidence. Use the hosted `doc` tool with topic `request` for
refusal and uncertain delivery; `doc request` is a hosted query, not Haskell
source for a notebook cell.
Use `exomonad-cleanup` for `stopAgent`, `planCleanup`, and `executeCleanup`.

A child reaches its own parent the same way, through `parentAgent`, not a
retained handle — it has none. `sendMessage` returns
`Either NotificationError NotificationReceipt`, a plain value to inspect, not
one to `respond` with. It is progress only: the receipt is admission evidence,
and the assignment stays open until the child calls `respond`.

```haskell
parentAgent >>= \case
  Nothing -> pure ()
  Just parent -> void (sendMessage parent "starting the migration; will check back before merging")
```

To check whether a lead's children (or their own children) have started, read
`Tidepool.Actors.Observe`'s `creationTree` (self identity from `actorContext`) over
`snapshot`, the same registry `observeAgent` reads, filtered to `RosterRunning` for
just the live ones.

Assignment values and explicit worktree seeds keep ordinary Haskell value
semantics and are not reevaluated at startup. A later failure in the cell stops
its suffix but preserves the unfolds that already succeeded. `doc unfold` holds
the same material in fallback form, with the budget and role details.
