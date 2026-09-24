---
name: exomonad-orchestrate
description: Put the implement/review/repair/merge loop inside one record actor so a child's outcome routes itself instead of costing the root a turn. Load when a root is relaying findings, collecting artifacts, or re-requesting review by hand.
---

Every transition whose next action is already determined by a child's outcome
belongs in Haskell, not in a model turn. Implementation settles → derive the
evidence and start the review. Review asks for repair → send the findings back.
Repair settles → re-review. Accepted → merge and run the integrated check.
Those were ten root turns per task in a measured run; they are one actor here.

**Find out whether an installed actor already fits, before writing any of it.**
`doc topics` ends by naming this workspace's own compiled modules, and `lookup`
on one of those names browses its declarations and the outcomes it can return.
The answer is often that the workspace authors types and helpers and no actor,
and then writing your own is exactly right. The cost is only in not checking:
one lead browsed a module, saw `RedRolledBack GitOid GitOid CheckResult` in the
answer, and then spent nine model turns and two and a half minutes writing
`tryMerge`, `cargo test` and `git reset --hard HEAD^` by hand — reimplementing,
badly, the outcome it had just been shown. Starting that actor was four calls.

**This is a pattern to copy into your own `.exomonad/Project`, not a library.**
Nothing below is importable. Paste it, rename it, cut the seams you do not
need. `.exomonad/workspace/checks/review-continuation.hs` is the executable
precedent for the review → repair → re-review half of it.

Availability of the names used here:

- **Shipped** — in every Exomonad cell: `R.definition`, `R.start`, `R.client`,
  `R.send`, `R.call`, `R.on`, `R.settlement`, `R.self`, `R.finish`, `R.withWorktree`,
  `LocalEffects`, `ActorSpec`, `Handler`, `Actor.Selected`, `knownEffects`,
  `Replies`, `Actor`, `Notifications`, `Forks`, `unfold`, `child`, `coding`,
  `assignment`, `atRef`, `GitRef`, `BranchName`, `mkBranchName`,
  `requestWithProgressInto`, `updateRequest`, `sendMessage`, `responseWorktree`,
  `renderGitOid`, `tryMerge`, `MergeRequest`, `Cmd.run`, `Cmd.withArguments`,
  `J.ask`, `J.ask1`, `J.choice`, `J.alt`, `J.many`, `J.settle`, `J.takenUnder`,
  `J.judge`, `J.explain`, `J.state`, `J.lenient`, `J.careful`, `J.strict`.
- **Needs an import in the cell**: `Jev` and `Commands` are effect types from
  `Tidepool.Effects.Core`, not re-exported by the workbench surface. Add
  `import Tidepool.Effects.Core (Jev, Commands)` before the row.
- **Project-authored** — you define these, they are shown here so you can copy
  them: `Contract`, `GateState`, `Wake`, `Gate`, `GateEffects`, `gateFor`,
  `gateView`.
- **Example-only** — present in `exomonad/examples/workspace`, absent from a fresh
  project: `coordinationActor`, `CoordinationEffects`, `Outcome`, `Candidate`.
  This skill does not use them. Confirm any name with `lookup` before you
  depend on it; a skill's example is not proof the name is installed.

## The seven wakes

The gate sends the root a message for exactly these, and for nothing else:

1. mechanical evidence fails unexpectedly (a named path has no hunk, or a file
   changed outside the owned paths);
2. any Jev answer comes back `Left Doubt` or picks `insufficient_evidence`;
3. a repair request that changes the contract rather than the code;
4. a merge conflict, or the integrator blocked on publication drift;
5. a failing integrated check after the merge (rolled back; a repair follows);
6. cleanup that retains a resource;
7. the finished outcome.

Everything else — collecting artifacts, starting a review, relaying findings,
re-reviewing, merging a clean candidate — happens without a turn. Each wake
names the decision that produced it, so the root's first act is a read, not
an investigation.

## The record

One `State`, one `Event` per child whose settlement drives the loop, one `Call`
the root reads the state through. The row is `Selected`, pinned by a signature
on `gateFor` — `knownEffects` alone is ambiguous.

Not executable on its own: it closes over a live `implementer` response.

```haskell
import GHC.Generics (Generic)
import Tidepool.Effects.Core (Jev, Commands)
data Contract = Contract { ownedPaths :: [Text], requiredTests :: [Text], likelyMiss :: Text, baseOid :: Text, onto :: BranchName, integrationTree :: WorktreeId, rootRef :: AgentRef }
data Wake = Wake { wakeReason :: Text, wakeSeam :: Text, wakeDetail :: Text }
instance Show Wake where show w = T.unpack (wakeReason w <> " [" <> wakeSeam w <> "] " <> wakeDetail w)
data GateState = GateState { gateContract :: Contract, gateEvidence :: [(Text, Text)], gateDecisions :: [Wake], gateRepairs :: Int }
instance Show GateState where show s = unlines (map show (gateDecisions s))
type GateEffects = LocalEffects Gate '[Replies, Watches, Forks, ActorContext, AgentInspection, BoundWorktree, Notifications, Jev, Commands, Actor]
data Gate mode = Gate
  { gateStateField :: mode :- State GateState
  , candidateSettled :: mode :- Event (Either ResponseFailure (ResponseResult Text))
  , reviewSettled :: mode :- Call (Either ResponseFailure (ResponseResult Text)) NoReply
  , gateView :: mode :- Call () (R.Reply GateState)
  } deriving Generic
```

`gateView` is the root's one call. `Show GateState` is the decision list: one
line per decision, so reading the whole state costs one short display.

## The handlers

The implementer's settlement carries a claim; the OID carries the evidence.
Derive the diff yourself, using the parent's owned handle, from the OID on the reply —
never from the child's own file list, and never by reading the child's
checkout. Check coverage and ownership in code *before* asking Jev anything: a
confident `all_present` on incomplete evidence is a measured failure mode, and
completeness is a git command, not a judgment.

Not executable on its own: it starts children and reads a live settlement.

```haskell
let gateFor :: Contract -> Response Text -> ActorSpec Gate GateEffects
    gateFor contract implementer = R.definition "gate" (Actor.Selected knownEffects) Gate
          { gateStateField = GateState contract [] [] 0
          , gateView = \() -> get
          , candidateSettled = R.on (R.settlement implementer) $ \settled -> case settled of
              Right receipt | WorktreeObserved _ submitted _ <- responseWorktree receipt -> do
                let oid = renderGitOid submitted
                let range = baseOid contract <> ".." <> oid
                stat <- either (const "") id . Cmd.stdout <$> Cmd.run (Cmd.withArguments [range] [bash|git diff --numstat "$1"|])
                hunks <- either (const "") id . Cmd.stdout <$> Cmd.run (Cmd.withArguments [range] [bash|git diff --unified=3 "$1"|])
                modify' (\s -> s { gateEvidence = gateEvidence s ++ [(oid, stat)] })
                let changed = [p | row <- T.lines stat, (_ : _ : p : _) <- [T.splitOn "\t" row]]
                let uncovered = [p | p <- changed, not (any (T.isInfixOf p) (T.lines hunks))]
                let outside = [p | p <- changed, p `notElem` ownedPaths contract]
                own <- R.self @Gate
                if not (null uncovered) || not (null outside)
                  then wake "evidence_incomplete" "coverage" (T.intercalate ", " (uncovered ++ outside))
                  else startReview own contract implementer oid stat hunks
              _ -> wake "child_did_not_submit" "settlement" (T.pack (show settled))
          , reviewSettled = \result -> do
              own <- R.self @Gate
              routeVerdict own result
          }
```

`wake`, `startReview` and `routeVerdict` are three more of your own handler
helpers. A separately bound helper needs its row written out — GHC cannot infer
it from the record — so give each one
`:: … -> Handler GateState GateEffects ()`, with the signature and its equation
in the same `let` item.

`startReview` admits the reviewer from inside the handler with `unfold`, at
depth 1, read-only, and hands it only the evidence fields the brief needs:

```
unfold group (child @Text (readonly (assignment reviewLabel brief)))
```

`routeVerdict` sends a repair back to the **same** child with
`updateRequest implementer findings` — the findings verbatim, not your
paraphrase — and counts the attempt. On accept it asks the integrator to
merge.

## The integrator owns the worktree

Worktree ownership is exclusive and integrate authority follows the owned
handle: only the actor bound to a worktree may `tryMerge` into it, and a record actor started
without a worktree resolves to the research role, whose ceiling has no
`WorktreeIntegration` at all. So merging is its own actor. The parent creates
the integration worktree and does not bind it; one `Integrator` is started
holding it with `R.withWorktree`; every gate `R.call`s that integrator, whose
mailbox serialises each merge-and-check:

```
Right tree <- createWorktree (fromRef "exomonad/integration" "integration")
integrator <- R.start (integratorFor (worktreeId tree) (Just "exomonad/integration"))
gate <- R.start (gateFor contract worker (Integration integrator))
```

The integrator checks before it publishes. Its one `Call` reads the worktree
head, confirms the publication branch is at that head and checked out nowhere
else, merges with `mergeAdvance = Nothing`, runs the project check on the
merged head, and only on green moves the branch with
`git update-ref refs/heads/<branch> <checked> <before>`. On red it resets the
worktree to `before`, so the worktree is on a green revision between calls and
the next queued candidate never builds on a red one. Publication drift, a
refused advance, or a rollback that did not restore the head put it in an
explicit blocked state: every further request is answered `IntegrationBlocked`
until the parent fixes the tree by hand and sends `reconcile`. Each outcome
names the revisions it checked or refused — a merge that landed and then
failed its check is a different fact from a merge that failed.

The gate's row therefore has no `WorktreeIntegration`:
`'[Replies, Watches, Forks, ActorContext, AgentInspection, BoundWorktree,
Notifications, Jev, Commands, Actor]`, with the read-only reviewer's row a
subset of it. The integrator's row is
`'[Replies, BoundWorktree, WorktreeIntegration, Commands, Actor]`.

A node in a recursive tree runs the same two actors from its own notebook: the
coding role may allocate, so it creates its own integration worktree from its
bound head, starts its own integrator, and replies to its parent with the same
typed report a leaf sends — integrated head, aggregate changed paths, literal
check output, unresolved conditions. The parent's gate then derives the whole
subtree diff from that head and checks it against the node's contract exactly
as it checks a leaf; nested ownership is the same `ownershipCheck` one level up.

## Coverage and ownership are code

This is the check that runs before any Jev call, and the one whose failure is
wake 1. It is ordinary text work on your own `git diff` output.

```haskell
let numstat = "3\t1\tsrc/app.rs\n12\t0\tsrc/panels/tags.rs" :: Text
let hunks = "diff --git a/src/app.rs b/src/app.rs\ndiff --git a/src/panels/tags.rs b/src/panels/tags.rs" :: Text
let owned = ["src/app.rs", "src/panels/tags.rs"] :: [Text]
let changed = [p | row <- T.lines numstat, (_ : _ : p : _) <- [T.splitOn "\t" row]]
let covered = all (\p -> any (T.isInfixOf p) (T.lines hunks)) changed
let outside = [p | p <- changed, p `notElem` owned]
(covered, changed, outside)
```

## One gate, with an exit

The playbook gate is a checklist `choice`, every option enumerating its own
conditions, plus a fourth option for evidence that cannot answer the question.
The pre-wave "most likely missed condition" for this task goes into
`item_missing` **verbatim**: a condition the option never names is a condition
the gate has nothing to check against, and that is exactly how a run-6
candidate was accepted at 0.90 while missing the empty-list case.

```haskell
let items = "every changed file is inside the owned paths; every required test name appears passing in the check output; the empty-list case of the new filter is handled" :: Text
let gate = J.choice "Which statement describes the candidate?"
      (J.alt #all_present ("Every item of the checklist holds: " <> items) ("merge" :: Text)
        J..| J.alt #item_missing "At least one item does not hold: a changed file outside the owned paths, a required test missing or failing in the check output, or the empty-list case of the new filter left unhandled" "repair"
        J..| J.alt #conflicting "The items are all present but contradict each other, for example the report claims a required test passes that the check output shows failing" "escalate"
        J..| J.alt #insufficient_evidence "The state does not carry what the checklist needs to be decided: a file named in `diff_stat` has no hunk, or `test_output` does not name the required tests at all" "ask again")
answer <- J.ask1 (J.state (#owned_paths := (["src/app.rs"] :: [Text]) :& #acceptance_checklist := ([items] :: [Text]) :& #base := ("abc1230" :: Text) :& #candidate := ("def4560" :: Text) :& #diff_stat := ("src/app.rs | 4 ++--" :: Text) :& #hunks := ("diff --git a/src/app.rs b/src/app.rs\n@@\n+    if items.is_empty() { return Vec::new(); }" :: Text) :& #test_output := ("test app::tests::tag_filter_empty ... ok\ntest result: ok. 18 passed; 0 failed" :: Text))) gate
case answer of
  Left err -> "jev unavailable: " <> T.pack (show err)
  Right a -> case J.takenUnder J.strict a of
    Left doubt -> "hold: " <> doubt.why
    Right (J.Settled action) -> a.key <> " -> " <> action <> "; " <> J.explain J.strict a
```

`insufficient_evidence` is not a failure of the candidate. It means the packet
was wrong, so the handler's reply is to name the exact missing field and send
it back — to the child with `updateRequest` when the child can supply it, to
the root as wake 2 when it cannot. Never merge on it and never repair on it.

The other seams take the same shape, each recorded in the state, each with the exit, each
under a named policy: unmatched check output → `J.lenient` over the
classification alternatives; reviewer findings → `J.lenient` over
`{addresses_named_checklist_item, contract_change_needed, style_only,
insufficient_evidence}`; "is this the same defect as the last one" → a Noul
over both texts read with `J.judge`, escalating on a repeat instead of a third
repair; which evidence the reviewer's brief needs → `J.lenient`; admitting a
reviewer at all → `J.careful`; the merge gate above → `J.strict`.

## The state is the wake

Every Jev call and every routing decision appends one `Wake`. The root reads
the whole loop with one `R.call (gateView (R.client gate)) ()`.

```haskell
data Wake = Wake { wakeReason :: Text, wakeSeam :: Text, wakeDetail :: Text }
instance Show Wake where show w = T.unpack (wakeReason w <> " [" <> wakeSeam w <> "] " <> wakeDetail w)
let decisions = [Wake "evidence_incomplete" "coverage" "src/store.rs named in the stat has no hunk", Wake "merged" "merge" "def4560 onto integration/tags"] :: [Wake]
map (T.pack . show) decisions
```

Keep the detail short and literal. A wake that needs the root to reconstruct
what happened costs the turn the gate was written to save.

Load `exomonad-define-actors` for the record syntax, `exomonad-jev` for the packet
and policy rules, and `exomonad-unfold` for reading a child's commit from your own
Git view.
