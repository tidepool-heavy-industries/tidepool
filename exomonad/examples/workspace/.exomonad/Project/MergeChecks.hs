{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.MergeChecks (redPreserved, greenReceipt, commandFailure, checkedHeadChanged, dirtyAfterSuccess, MergeProbe (probeCommand), mergeProbe) where

import Prelude hiding (writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Data.Text (Text)
import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad hiding (checkpoint)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands)
import Tidepool.Effects.Row (knownEffects)
import Tidepool.Check

-- Fixture commands use the managed checkout's authority and return the exact
-- original terminal receipt and capture to the recipe actor.
data MergeProbe mode = MergeProbe
  { probeState :: mode :- State ()
  , probeCommand :: mode :- Call [Text] (R.Reply Cmd.RunResult)
  } deriving Generic

type MergeProbeEffects = LocalEffects MergeProbe '[Replies, Commands]

mergeProbe :: ActorSpec MergeProbe MergeProbeEffects
mergeProbe = R.definition "merge-check-fixture" (Actor.Selected knownEffects) MergeProbe
  { probeState = ()
  , probeCommand = Cmd.run . Cmd.withMemory (Cmd.MiB 64) . Cmd.argv
  }

-- A failed integration check leaves its staged and unstaged diagnostics in
-- the managed checkout while the named publication branch stays at the
-- previous green head. Its next publish is refused by branch drift.
redPreserved :: Member RecipeCheck effects => Eff effects ()
redPreserved = do
  owner <- root
  before <- git owner ["rev-parse", "HEAD"]
  let branch = "recipe/red-preserved"
  void $ git owner ["branch", branch, before]
  void $ turn owner $ Text.unlines
    [ "import qualified Exomonad.Contrib.Merge as M"
    , "import qualified Project.MergeChecks as Fixture"
    , "import Tidepool.Worktree (workspaceFor)"
    , "Right sourceTree <- createWorktree (fromRef \"recipe/red-preserved\" \"red-source\")"
    , "Right integration <- createWorktree (fromRef \"recipe/red-preserved\" \"red-preserved\")"
    , "Right sourceWorkspace <- workspaceFor sourceTree"
    , "Right integrationWorkspace <- workspaceFor integration"
    , "Right integrationProbeWorkspace <- workspaceFor integration"
    , "merger <- R.start (M.mergeInto integrationWorkspace (Just \"recipe/red-preserved\") [\"sh\", \"-c\", \"printf 'staged-check\\n' > red-preserved.txt; git add -- red-preserved.txt; printf 'working-check\\n' > red-preserved.txt; printf intentional-red >&2; exit 7\"])"
    , "sourceProbe <- R.start (R.withWorkspace sourceWorkspace Fixture.mergeProbe)"
    , "integrationProbe <- R.start (R.withWorkspace integrationProbeWorkspace Fixture.mergeProbe)"
    ]
  void $ turn owner $ Text.unlines
    [ "let commandAt :: R.ActorHandle Fixture.MergeProbe -> [Text] -> Eff RootEffects Cmd.RunResult; commandAt probe args = R.call (Fixture.probeCommand (R.client probe)) args"
    , "let gitAt :: R.ActorHandle Fixture.MergeProbe -> [Text] -> Eff RootEffects Cmd.RunResult; gitAt probe args = commandAt probe ([\"git\"] ++ args)"
    , "reflogBefore <- gitAt integrationProbe [\"reflog\", \"--format=%gs\"]"
    , "sourceEdit <- commandAt sourceProbe [\"sh\", \"-c\", \"printf 'candidate\\n' > red-preserved.txt\"]"
    , "sourceAdded <- gitAt sourceProbe [\"add\", \"--\", \"red-preserved.txt\"]"
    , "sourceCommitted <- gitAt sourceProbe [\"-c\", \"user.name=Recipe\", \"-c\", \"user.email=recipe@example.invalid\", \"commit\", \"-m\", \"red preservation candidate\", \"--\", \"red-preserved.txt\"]"
    , "Right candidate <- worktreeHead sourceTree"
    ]
  assertCell owner "red fixture commits exact source with clean command receipts"
    "all (\\result -> Cmd.commandResult result == Cmd.CommandResult (Cmd.CommandExited 0) Cmd.CommandClean) [sourceEdit, sourceAdded, sourceCommitted]"
  void $ turn owner $ Text.unlines
    [ "let request = M.PublishRequest \"red preservation\" (worktreeId sourceTree) candidate \"merge red preservation candidate\""
    , "first <- R.call (M.publish (R.client merger)) request"
    ]
  void $ turn owner $ Text.unlines
    [ "view <- R.call (M.mergeView (R.client merger)) ()"
    ]
  assertCell owner "integration actor owns the allocated managed checkout"
    "case M.mergeTree view of { Just tree -> worktreeId tree == worktreeId integration; _ -> False }"
  assertCell owner "red evidence retains terminal receipt and same history proof"
    ("(case first of { M.RedPreserved _ checked evidence -> M.integrationHead evidence == checked && not (M.integrationPassed evidence) && Cmd.commandOutcome (Cmd.commandResult (M.integrationReceipt evidence)) == Cmd.CommandExited 7 && Cmd.commandCleanup (Cmd.commandResult (M.integrationReceipt evidence)) == Cmd.CommandClean && case reverse (M.mergeHistory view) of { M.MergeHistory _ _ (M.IntegrationRed _ retained) : _ -> retained == evidence; _ -> False }; _ -> False })")
  void $ turn owner "postEdit <- commandAt integrationProbe [\"sh\", \"-c\", \"printf 'post-check-edit\\n' > post-check.txt\"]"
  assertCell owner "later edit fixture completes cleanly"
    "Cmd.commandResult postEdit == Cmd.CommandResult (Cmd.CommandExited 0) Cmd.CommandClean"
  void $ turn owner $ Text.unlines
    [ "headAfter <- worktreeHead integration"
    , "second <- R.call (M.publish (R.client merger)) request"
    , "secondView <- R.call (M.mergeView (R.client merger)) ()"
    , "published <- gitAt sourceProbe [\"rev-parse\", \"refs/heads/recipe/red-preserved\"]"
    , "status <- gitAt integrationProbe [\"status\", \"--short\", \"--\", \"red-preserved.txt\"]"
    , "staged <- gitAt integrationProbe [\"diff\", \"--cached\", \"--\", \"red-preserved.txt\"]"
    , "working <- gitAt integrationProbe [\"diff\", \"--\", \"red-preserved.txt\"]"
    , "reflog <- gitAt integrationProbe [\"reflog\", \"--format=%gs\"]"
    , "later <- gitAt integrationProbe [\"status\", \"--short\", \"--\", \"post-check.txt\"]"
    ]
  assertCell owner "red result retains previous and checked heads plus original failed receipt"
    ("case first of { M.RedPreserved previous checked evidence -> previous == " <> gitOidLiteral before <> " && checked == candidate && M.integrationHead evidence == candidate && Cmd.stderr (M.integrationReceipt evidence) == Right \"intentional-red\"; _ -> False }")
  assertCell owner "red head remains and next publish refuses with no second check"
    "headAfter == Right candidate && case (second, M.mergeHistory secondView) of { (M.MergeBlocked _, [M.MergeHistory _ _ (M.IntegrationRed _ original), M.MergeHistory _ _ (M.PublicationBlocked _)]) -> case first of { M.RedPreserved _ _ evidence -> original == evidence; _ -> False }; _ -> False }"
  assertCell owner "named publication branch stays at previous green head"
    ("fmap T.strip (Cmd.stdout published) == Right (renderGitOid " <> gitOidLiteral before <> ")")
  assertCell owner "text: staged and working diagnostic edits survive failed check"
    "fmap T.strip (Cmd.stdout status) == Right \"MM red-preserved.txt\" && case (Cmd.stdout staged, Cmd.stdout working) of { (Right stagedText, Right workingText) -> \"staged-check\" `T.isInfixOf` stagedText && \"working-check\" `T.isInfixOf` workingText; _ -> False }"
  assertCell owner "text: failed check never records destructive reset in Git reflog"
    "case (Cmd.stdout reflogBefore, Cmd.stdout reflog) of { (Right before, Right after) -> not (any (T.isInfixOf \"reset:\") (take (length (T.lines after) - length (T.lines before)) (T.lines after))); _ -> False }"
  assertCell owner "text: refused later publish preserves post-check edit"
    "fmap T.strip (Cmd.stdout later) == Right \"?? post-check.txt\""
  void $ turn owner "R.finish merger\nR.finish sourceProbe\nR.finish integrationProbe"

-- A command exit is integration evidence, even when it executes no tests. Its
-- exact argv, checked head, outcome and original output stay separately usable.
greenReceipt :: Member RecipeCheck effects => Eff effects ()
greenReceipt = do
  owner <- root
  before <- checkpoint owner ".gitignore" "ignored-build/\n" "ignore disposable check output"
  void $ turn owner $ Text.unlines
    [ "import qualified Exomonad.Contrib.Merge as M"
    , "import qualified Project.MergeChecks as Fixture"
    , "import Tidepool.Worktree (workspaceFor)"
    , "Right sourceTree <- createWorktree (fromRef \"HEAD\" \"green-source\")"
    , "Right integration <- createWorktree (fromRef \"HEAD\" \"green-receipt\")"
    , "Right integrationWorkspace <- workspaceFor integration"
    , "let command = [\"sh\", \"-c\", \"mkdir -p ignored-build; printf artifact > ignored-build/output; printf zero-tests\"]"
    , "merger <- R.start (M.mergeInto integrationWorkspace Nothing command)"
    , "result <- R.call (M.publish (R.client merger)) (M.PublishRequest \"green receipt\" (worktreeId sourceTree) " <> gitOidLiteral before <> " \"green receipt\")"
    , "view <- R.call (M.mergeView (R.client merger)) ()"
    ]
  assertCell owner "green publication retains exact command evidence without claiming tests"
    ("(case result of { M.Published checked previous evidence -> checked == " <> gitOidLiteral before <> " && previous == checked && M.integrationHead evidence == checked && M.integrationArgv evidence == command && M.integrationPassed evidence && Cmd.stdout (M.integrationReceipt evidence) == Right \"zero-tests\" && case reverse (M.mergeHistory view) of { M.MergeHistory _ _ (M.IntegrationPublished _ retained) : _ -> retained == evidence; _ -> False }; _ -> False })")
  assertCell owner "retained, unknown cleanup and failed exit cannot be green"
    ("(case result of { M.Published _ _ evidence -> let original = M.integrationReceipt evidence; completion = Cmd.commandResult original; retained = original { Cmd.commandResult = completion { Cmd.commandCleanup = Cmd.CommandRetained } }; unknown = original { Cmd.commandResult = completion { Cmd.commandCleanup = Cmd.CommandCleanupUnknown \"unconfirmed\" } }; failed = original { Cmd.commandResult = completion { Cmd.commandOutcome = Cmd.CommandExited 9 } } in all (not . M.integrationPassed) [evidence { M.integrationReceipt = retained }, evidence { M.integrationReceipt = unknown }, evidence { M.integrationReceipt = failed }]; _ -> False })")
  void $ turn owner "R.finish merger"

-- A failed pre-merge Git lookup must remain a failure, never become an empty
-- OID that appears to agree with another failed lookup.
commandFailure :: Member RecipeCheck effects => Eff effects ()
commandFailure = do
  owner <- root
  before <- git owner ["rev-parse", "HEAD"]
  void $ turn owner $ Text.unlines
    [ "import qualified Exomonad.Contrib.Merge as M"
    , "import qualified Project.MergeChecks as Fixture"
    , "import Tidepool.Worktree (workspaceFor)"
    , "Right sourceTree <- createWorktree (fromRef \"HEAD\" \"git-failure-source\")"
    , "Right integration <- createWorktree (fromRef \"HEAD\" \"git-failure\")"
    , "Right integrationWorkspace <- workspaceFor integration"
    , "merger <- R.start (M.mergeInto integrationWorkspace (Just \"recipe/missing-publication-branch\") [\"sh\", \"-c\", \"touch should-not-run\"])"
    , "let request = M.PublishRequest \"git failure\" (worktreeId sourceTree) " <> gitOidLiteral before <> " \"git failure\""
    , "result <- R.call (M.publish (R.client merger)) request"
    , "headAfter <- worktreeHead integration"
    , "view <- R.call (M.mergeView (R.client merger)) ()"
    ]
  assertCell owner "failed Git lookup preserves head and admits no integration command"
    ("(headAfter == Right " <> gitOidLiteral before <> " && case result of { M.MergeFailed _ -> case M.mergeHistory view of { [M.MergeHistory _ candidate (M.PublishFailed _)] -> candidate == Just (M.publishCandidate request); _ -> False }; _ -> False })")
  void $ turn owner "Right integrationProbeWorkspace <- workspaceFor integration\nintegrationProbe <- R.start (R.withWorkspace integrationProbeWorkspace Fixture.mergeProbe)\nstatus <- R.call (Fixture.probeCommand (R.client integrationProbe)) [\"git\", \"status\", \"--short\"]\nR.finish integrationProbe"
  assertCell owner "integration command never ran after failed Git lookup"
    "Cmd.stdout status == Right \"\""
  void $ turn owner "R.finish merger"

-- A successful command that changes HEAD has not checked the head it leaves
-- behind. Keep its original proof and block publication until reconciliation.
checkedHeadChanged :: Member RecipeCheck effects => Eff effects ()
checkedHeadChanged = do
  owner <- root
  before <- git owner ["rev-parse", "HEAD"]
  let branch = "recipe/changed-check-head"
  void $ git owner ["branch", branch, before]
  void $ turn owner $ Text.unlines
    [ "import qualified Exomonad.Contrib.Merge as M"
    , "import qualified Project.MergeChecks as Fixture"
    , "import Tidepool.Worktree (workspaceFor)"
    , "Right sourceTree <- createWorktree (fromRef \"HEAD\" \"changed-head-source\")"
    , "Right integration <- createWorktree (fromRef \"HEAD\" \"changed-head\")"
    , "Right integrationWorkspace <- workspaceFor integration"
    , "merger <- R.start (M.mergeInto integrationWorkspace (Just \"recipe/changed-check-head\") [\"git\", \"-c\", \"user.name=Recipe\", \"-c\", \"user.email=recipe@example.invalid\", \"commit\", \"--allow-empty\", \"-m\", \"unchecked command commit\"])"
    , "result <- R.call (M.publish (R.client merger)) (M.PublishRequest \"changed head\" (worktreeId sourceTree) " <> gitOidLiteral before <> " \"changed head\")"
    , "view <- R.call (M.mergeView (R.client merger)) ()"
    , "headAfter <- worktreeHead integration"
    ]
  assertCell owner "changed HEAD blocks publication retaining original checked receipt"
    ("(headAfter /= Right " <> gitOidLiteral before <> " && case (result, reverse (M.mergeHistory view)) of { (M.MergeBlocked _, M.MergeHistory _ _ (M.IntegrationBlocked evidence _) : _) -> M.integrationHead evidence == " <> gitOidLiteral before <> " && M.integrationPassed evidence; _ -> False })")
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
  void $ turn owner $ Text.unlines
    [ "import qualified Exomonad.Contrib.Merge as M"
    , "import qualified Project.MergeChecks as Fixture"
    , "import Tidepool.Worktree (SubmissionObservation (..), WorkingState (..), DirtySummary (..), workspaceFor)"
    , "Right sourceTree <- createWorktree (fromRef \"HEAD\" \"dirty-success-source\")"
    , "Right integration <- createWorktree (fromRef \"HEAD\" \"dirty-success\")"
    , "Right integrationWorkspace <- workspaceFor integration"
    , "let command = [\"sh\", \"-c\", \"printf staged > checked-source.txt; git add -- checked-source.txt; printf working > checked-source.txt; printf untracked > unchecked-source.txt; printf successful-dirty-check\"]"
    , "merger <- R.start (M.mergeInto integrationWorkspace (Just \"recipe/dirty-success\") command)"
    , "result <- R.call (M.publish (R.client merger)) (M.PublishRequest \"dirty source\" (worktreeId sourceTree) " <> gitOidLiteral before <> " \"dirty source\")"
    , "view <- R.call (M.mergeView (R.client merger)) ()"
    , "headAfter <- worktreeHead integration"
    ]
  assertCell owner "successful command retains dirty source proof and blocks publication"
    ("(headAfter == Right " <> gitOidLiteral before <> " && case (result, reverse (M.mergeHistory view)) of { (M.MergeBlocked _, M.MergeHistory _ _ (M.IntegrationBlocked evidence _) : _) -> M.integrationHead evidence == " <> gitOidLiteral before <> " && M.integrationArgv evidence == command && M.integrationPassed evidence && Cmd.stdout (M.integrationReceipt evidence) == Right \"successful-dirty-check\" && case M.integrationSubmission evidence of { Just submission -> case changes (workingState submission) of { DirtySummary staged unstaged untracked _ -> all (not . null) [staged, unstaged, untracked] }; Nothing -> False }; _ -> False })")
  published <- git owner ["rev-parse", "refs/heads/" <> branch]
  check "dirty tested source is not published as its original commit" (published == before)
  void $ turn owner "Right integrationProbeWorkspace <- workspaceFor integration\nintegrationProbe <- R.start (R.withWorkspace integrationProbeWorkspace Fixture.mergeProbe)\nstatus <- R.call (Fixture.probeCommand (R.client integrationProbe)) [\"git\", \"status\", \"--short\"]\nR.finish integrationProbe"
  assertCell owner "text: blocked publication preserves tracked and untracked diagnostic edits"
    "case Cmd.stdout status of { Right text -> \"MM checked-source.txt\" `T.isInfixOf` text && \"?? unchecked-source.txt\" `T.isInfixOf` text; _ -> False }"
  void $ turn owner "R.finish merger"
