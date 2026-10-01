{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.MergeChecks (redPreserved, greenReceipt, commandFailure, checkedHeadChanged, dirtyAfterSuccess) where

import Prelude hiding (writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check

-- A failed integration check leaves its staged and unstaged diagnostics in
-- the managed checkout while the named publication branch stays at the
-- previous green head. Its next publish is refused by branch drift.
redPreserved :: Member RecipeCheck effects => Eff effects ()
redPreserved = do
  owner <- root
  before <- git owner ["rev-parse", "HEAD"]
  ownerPath <- git owner ["rev-parse", "--show-toplevel"]
  let externalEdit directory name line = do
        writeFile owner "fixture.patch" (Text.unlines
          [ "diff --git a/" <> name <> " b/" <> name
          , "new file mode 100644", "--- /dev/null", "+++ b/" <> name
          , "@@ -0,0 +1 @@", "+" <> line ])
        void $ git owner ["-C", directory, "apply", ownerPath <> "/fixture.patch"]
  let branch = "recipe/red-preserved"
  void $ git owner ["branch", branch, before]
  created <- turn owner $ Text.unlines
    [ "import qualified Exomonad.Contrib.Merge as M"
    , "Right sourceTree <- createWorktree (fromRef \"recipe/red-preserved\" \"red-source\")"
    , "Right integration <- createWorktree (fromRef \"recipe/red-preserved\" \"red-preserved\")"
    , "merger <- R.start (M.mergeInto (worktreeId integration) (Just \"recipe/red-preserved\") [\"sh\", \"-c\", \"printf 'staged-check\\n' > red-preserved.txt; git add -- red-preserved.txt; printf 'working-check\\n' > red-preserved.txt; printf intentional-red >&2; exit 7\"])"
    , "worktreeId integration"
    ]
  check "the integration actor owns a managed checkout"
    ("WorktreeId" `Text.isInfixOf` lastOutput created)
  pathResult <- turn owner "cwd (handleReceipt integration)"
  let integrationPath = Text.dropAround (== '"') (Text.strip (lastOutput pathResult))
  reflogBefore <- git owner ["-C", integrationPath, "reflog", "--format=%gs"]
  sourcePathResult <- turn owner "cwd (handleReceipt sourceTree)"
  let sourcePath = Text.dropAround (== '"') (Text.strip (lastOutput sourcePathResult))
  externalEdit sourcePath "red-preserved.txt" "candidate"
  void $ git owner ["-C", sourcePath, "add", "--", "red-preserved.txt"]
  void $ git owner ["-C", sourcePath, "commit", "-m", "red preservation candidate", "--", "red-preserved.txt"]
  candidate <- git owner ["-C", sourcePath, "rev-parse", "HEAD"]
  void $ turn owner $ Text.unlines
    [ "let request = M.PublishRequest \"red preservation\" (worktreeId sourceTree) " <> gitOidLiteral candidate <> " \"merge red preservation candidate\""
    , "first <- R.call (M.publish (R.client merger)) request"
    ]
  receipt <- turn owner $ Text.unlines
    [ "view <- R.call (M.mergeView (R.client merger)) ()"
    , "inspectFull (case first of { M.RedPreserved _ checked evidence -> M.integrationHead evidence == checked && not (M.integrationPassed evidence) && Cmd.commandOutcome (Cmd.commandResult (M.integrationReceipt evidence)) == Cmd.CommandExited 7 && Cmd.commandCleanup (Cmd.commandResult (M.integrationReceipt evidence)) == Cmd.CommandClean && case reverse (M.mergeHistory view) of { M.MergeHistory _ _ (M.IntegrationRed _ retained) : _ -> retained == evidence; _ -> False }; _ -> False })"
    ]
  check "red evidence retains the actual terminal receipt and the same proof in history"
    (lastOutput receipt == "True")
  externalEdit integrationPath "post-check.txt" "post-check-edit"
  result <- turn owner $ Text.unlines
    [ "headAfter <- worktreeHead integration"
    , "second <- R.call (M.publish (R.client merger)) request"
    , "(first, headAfter == Right " <> gitOidLiteral candidate <> ", second)"
    ]
  let observed = lastOutput result
  check "the red result retains the previous and checked heads plus failed check evidence"
    (all (`Text.isInfixOf` observed)
      ["RedPreserved", before, candidate, "intentional-red"])
  retained <- turn owner $ "inspectFull (headAfter == Right " <> gitOidLiteral candidate
    <> " && case second of { M.MergeBlocked reason -> \"publication branch\" `T.isInfixOf` reason; _ -> False })"
  check ("the red head is retained and next publish refused; observed: " <> observed)
    (lastOutput retained == "True")
  published <- git owner ["rev-parse", "refs/heads/" <> branch]
  check "the named publication branch remains at the previous green head"
    (published == before)
  status <- git owner ["-C", integrationPath, "status", "--short", "--", "red-preserved.txt"]
  staged <- git owner ["-C", integrationPath, "diff", "--cached", "--", "red-preserved.txt"]
  working <- git owner ["-C", integrationPath, "diff", "--", "red-preserved.txt"]
  check "the failed check's staged and working edits survive in the integration checkout"
    (status == "MM red-preserved.txt"
      && "staged-check" `Text.isInfixOf` staged
      && "working-check" `Text.isInfixOf` working)
  reflog <- git owner ["-C", integrationPath, "reflog", "--format=%gs"]
  check "the red path never records a destructive reset"
    (not (any (Text.isInfixOf "reset:")
      (take (length (Text.lines reflog) - length (Text.lines reflogBefore)) (Text.lines reflog))))

  later <- git owner ["-C", integrationPath, "status", "--short", "--", "post-check.txt"]
  check "a later refused publish preserves edits made after the failed check"
    (later == "?? post-check.txt")
  void $ turn owner "R.finish merger"

-- A command exit is integration evidence, even when it executes no tests. Its
-- exact argv, checked head, outcome and original output stay separately usable.
greenReceipt :: Member RecipeCheck effects => Eff effects ()
greenReceipt = do
  owner <- root
  before <- checkpoint owner ".gitignore" "ignored-build/\n" "ignore disposable check output"
  void $ turn owner $ Text.unlines
    [ "import qualified Exomonad.Contrib.Merge as M"
    , "Right sourceTree <- createWorktree (fromRef \"HEAD\" \"green-source\")"
    , "Right integration <- createWorktree (fromRef \"HEAD\" \"green-receipt\")"
    , "let command = [\"sh\", \"-c\", \"mkdir -p ignored-build; printf artifact > ignored-build/output; printf zero-tests\"]"
    , "merger <- R.start (M.mergeInto (worktreeId integration) Nothing command)"
    , "result <- R.call (M.publish (R.client merger)) (M.PublishRequest \"green receipt\" (worktreeId sourceTree) " <> gitOidLiteral before <> " \"green receipt\")"
    , "view <- R.call (M.mergeView (R.client merger)) ()"
    ]
  retained <- turn owner $ Text.unlines
    [ "inspectFull (case result of { M.Published checked previous evidence -> checked == " <> gitOidLiteral before <> " && previous == checked && M.integrationHead evidence == checked && M.integrationArgv evidence == command && M.integrationPassed evidence && Cmd.stdout (M.integrationReceipt evidence) == Right \"zero-tests\" && case reverse (M.mergeHistory view) of { M.MergeHistory _ _ (M.IntegrationPublished _ retained) : _ -> retained == evidence; _ -> False }; _ -> False })"
    ]
  check "green publication retains exact command evidence without claiming test execution"
    (lastOutput retained == "True")
  classified <- turn owner $ Text.unlines
    [ "inspectFull (case result of { M.Published _ _ evidence -> let original = M.integrationReceipt evidence; completion = Cmd.commandResult original; retained = original { Cmd.commandResult = completion { Cmd.commandCleanup = Cmd.CommandRetained } }; unknown = original { Cmd.commandResult = completion { Cmd.commandCleanup = Cmd.CommandCleanupUnknown \"unconfirmed\" } }; failed = original { Cmd.commandResult = completion { Cmd.commandOutcome = Cmd.CommandExited 9 } } in all (not . M.integrationPassed) [evidence { M.integrationReceipt = retained }, evidence { M.integrationReceipt = unknown }, evidence { M.integrationReceipt = failed }]; _ -> False })"
    ]
  check "retained cleanup, unknown cleanup and nonzero exit cannot be green"
    (lastOutput classified == "True")
  void $ turn owner "R.finish merger"

-- A failed pre-merge Git lookup must remain a failure, never become an empty
-- OID that appears to agree with another failed lookup.
commandFailure :: Member RecipeCheck effects => Eff effects ()
commandFailure = do
  owner <- root
  before <- git owner ["rev-parse", "HEAD"]
  observed <- turn owner $ Text.unlines
    [ "import qualified Exomonad.Contrib.Merge as M"
    , "Right sourceTree <- createWorktree (fromRef \"HEAD\" \"git-failure-source\")"
    , "Right integration <- createWorktree (fromRef \"HEAD\" \"git-failure\")"
    , "merger <- R.start (M.mergeInto (worktreeId integration) (Just \"recipe/missing-publication-branch\") [\"sh\", \"-c\", \"touch should-not-run\"])"
    , "result <- R.call (M.publish (R.client merger)) (M.PublishRequest \"git failure\" (worktreeId sourceTree) " <> gitOidLiteral before <> " \"git failure\")"
    , "headAfter <- worktreeHead integration"
    , "inspectFull (headAfter == Right " <> gitOidLiteral before <> " && case result of { M.MergeFailed reason -> \"git rev-parse\" `T.isInfixOf` reason && \"CommandExited\" `T.isInfixOf` reason; _ -> False })"
    ]
  check "failed Git lookup produces MergeFailed and preserves the head"
    (lastOutput observed == "True")
  pathResult <- turn owner "cwd (handleReceipt integration)"
  let path = Text.dropAround (== '"') (Text.strip (lastOutput pathResult))
  status <- git owner ["-C", path, "status", "--short"]
  check "the integration command never ran after the failed Git lookup" (Text.null status)
  void $ turn owner "R.finish merger"

-- A successful command that changes HEAD has not checked the head it leaves
-- behind. Keep its original proof and block publication until reconciliation.
checkedHeadChanged :: Member RecipeCheck effects => Eff effects ()
checkedHeadChanged = do
  owner <- root
  before <- git owner ["rev-parse", "HEAD"]
  let branch = "recipe/changed-check-head"
  void $ git owner ["branch", branch, before]
  observed <- turn owner $ Text.unlines
    [ "import qualified Exomonad.Contrib.Merge as M"
    , "Right sourceTree <- createWorktree (fromRef \"HEAD\" \"changed-head-source\")"
    , "Right integration <- createWorktree (fromRef \"HEAD\" \"changed-head\")"
    , "merger <- R.start (M.mergeInto (worktreeId integration) (Just \"recipe/changed-check-head\") [\"git\", \"-c\", \"user.name=Recipe\", \"-c\", \"user.email=recipe@example.invalid\", \"commit\", \"--allow-empty\", \"-m\", \"unchecked command commit\"])"
    , "result <- R.call (M.publish (R.client merger)) (M.PublishRequest \"changed head\" (worktreeId sourceTree) " <> gitOidLiteral before <> " \"changed head\")"
    , "view <- R.call (M.mergeView (R.client merger)) ()"
    , "headAfter <- worktreeHead integration"
    , "inspectFull (headAfter /= Right " <> gitOidLiteral before <> " && case (result, reverse (M.mergeHistory view)) of { (M.MergeBlocked reason, M.MergeHistory _ _ (M.IntegrationBlocked evidence _) : _) -> \"changed HEAD\" `T.isInfixOf` reason && M.integrationHead evidence == " <> gitOidLiteral before <> " && M.integrationPassed evidence; _ -> False })"
    ]
  check "changed HEAD blocks publication while the original successful receipt remains retained"
    (lastOutput observed == "True")
  published <- git owner ["rev-parse", "refs/heads/" <> branch]
  check "the unchecked command commit was not published" (published == before)
  void $ turn owner "R.finish merger"

-- Tracked, index and untracked edits cannot be passed off as the unchanged
-- commit, even when the project command exits successfully.
dirtyAfterSuccess :: Member RecipeCheck effects => Eff effects ()
dirtyAfterSuccess = do
  owner <- root
  before <- checkpoint owner "checked-source.txt" "committed source\n" "source before successful dirty check"
  let branch = "recipe/dirty-success"
  void $ git owner ["branch", branch, before]
  observed <- turn owner $ Text.unlines
    [ "import qualified Exomonad.Contrib.Merge as M"
    , "import Tidepool.Worktree (SubmissionObservation (..), WorkingState (..), DirtySummary (..))"
    , "Right sourceTree <- createWorktree (fromRef \"HEAD\" \"dirty-success-source\")"
    , "Right integration <- createWorktree (fromRef \"HEAD\" \"dirty-success\")"
    , "let command = [\"sh\", \"-c\", \"printf staged > checked-source.txt; git add -- checked-source.txt; printf working > checked-source.txt; printf untracked > unchecked-source.txt; printf successful-dirty-check\"]"
    , "merger <- R.start (M.mergeInto (worktreeId integration) (Just \"recipe/dirty-success\") command)"
    , "result <- R.call (M.publish (R.client merger)) (M.PublishRequest \"dirty source\" (worktreeId sourceTree) " <> gitOidLiteral before <> " \"dirty source\")"
    , "view <- R.call (M.mergeView (R.client merger)) ()"
    , "headAfter <- worktreeHead integration"
    , "inspectFull (headAfter == Right " <> gitOidLiteral before <> " && case (result, reverse (M.mergeHistory view)) of { (M.MergeBlocked reason, M.MergeHistory _ _ (M.IntegrationBlocked evidence _) : _) -> \"dirty or in progress\" `T.isInfixOf` reason && M.integrationHead evidence == " <> gitOidLiteral before <> " && M.integrationArgv evidence == command && M.integrationPassed evidence && Cmd.stdout (M.integrationReceipt evidence) == Right \"successful-dirty-check\" && case M.integrationSubmission evidence of { Just submission -> case changes (workingState submission) of { DirtySummary staged unstaged untracked _ -> all (not . null) [staged, unstaged, untracked] }; Nothing -> False }; _ -> False })"
    ]
  check "successful command with unchanged HEAD retains dirty source proof and blocks publication"
    (lastOutput observed == "True")
  published <- git owner ["rev-parse", "refs/heads/" <> branch]
  check "dirty tested source is not published as its original commit" (published == before)
  pathResult <- turn owner "cwd (handleReceipt integration)"
  let path = Text.dropAround (== '"') (Text.strip (lastOutput pathResult))
  status <- git owner ["-C", path, "status", "--short"]
  check "blocked publication preserves both tracked and untracked check edits"
    ("MM checked-source.txt" `Text.isInfixOf` status && "?? unchecked-source.txt" `Text.isInfixOf` status)
  void $ turn owner "R.finish merger"
