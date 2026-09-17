---
name: shoal-unfold
description: Admit several Shoal children in one applicative unfold, join their settlements in one watch, and read a child's committed work from the parent's own Git view. Load when decomposing work across children and inspecting what they produced.
---

`unfold` admits persistent context forks and returns their handles now. The
children start after the entire enclosing cell finishes, and they inherit that
cell's final committed bindings plus the conversation through the real tool
result. Combine independent children applicatively in one `unfold`; keep the
shared contract and the integration with their parent.

```haskell
let group = batch "corpus" "fanout"
let domainLabel = "domain" :: Label
let consumerLabel = "consumer-tests" :: Label
let domainPlan = "Add the shared item type and its tests." :: Text
let consumerPlan = "Update the readers of that type." :: Text
workers <- unfold group $
  (,) <$> child @(Outcome Candidate) (coding projectHead (assignment domainLabel domainPlan))
      <*> child @(Outcome Candidate) (withEffort Medium (coding projectHead (assignment consumerLabel consumerPlan)))
let sharedAfterUnfold = ("ready" :: Text)
```

Both children see `sharedAfterUnfold` although it is bound after the `unfold`.
Never await a child inside the cell that admits it: a queued child cannot start
until the cell returns. End that cell promptly.

`spawnWatched` admits and watches exactly one child, so it cannot take the pair
above. Admit with `unfold`, then join the settlements in one watch:

```haskell
joined <- watch "both-ready" $
  (,) <$> awaitSettled (fst workers) <*> awaitSettled (snd workers)
pollWatch joined
```

Use `awaitSettled` when a failure belongs in the value, `awaitResponse` when an
unavailable dependency should fail the watch, and `awaitAnySettled` to wake on
the first. Register the watch, then end the model round; a wake is a reason to
inspect retained handles, not proof of success.

## The child's checkout is not yours to read

A `Response` carries an immutable launch receipt on its launch request, and
that receipt's `cwd` names the child's own checkout. **It is not a path the
parent can read while the child lives.** The child owns that worktree; its
contents are mid-edit, and a path that resolves in your shell is a different
directory or a stale one. Nothing about holding the handle grants file access.

What you may use is the identity in the receipt — the branch and the commit —
resolved against the repository from your own view:

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
equivalents when you hold a worktree handle, and `tryMerge` integrates a
submission once you have decided to.

## Artifacts travel in the reply

A child's product reaches you as its typed settlement value — candidates,
findings, check evidence, unresolved decisions — plus the commit that backs it.
Do not ask a child to copy files into a shared directory, and do not go looking
for them: define the result type so the artifact is a value, and let the Git
identity carry anything too large to be one. A typed reply is evidence of
execution, not of integration; verify the submitted commit before merging.

Assignment values and explicit worktree seeds keep ordinary Haskell value
semantics and are not reevaluated at startup. A later failure in the cell stops
its suffix but preserves the unfolds that already succeeded. `doc unfold` holds
the same material in fallback form, with the budget and role details.
