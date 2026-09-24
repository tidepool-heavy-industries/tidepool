{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.SkillChecks (skills) where

import Prelude hiding (readFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Exomonad.Workspace (workspaceRoot)
import Tidepool.Check
import Tidepool.Aeson (Value)

-- Execute the skill's actual code blocks, not separately maintained copies.
example :: Member RecipeCheck effects => CheckActor -> Text -> Int -> Eff effects Value
example actor skill index = do
  body <- readFile actor (Text.pack workspaceRoot <> "/skills/" <> skill <> "/SKILL.md")
  let blocks = map (fst . Text.breakOn "```") (drop 1 (Text.splitOn "```haskell\n" body))
  turn actor (blocks !! index)

skills :: Member RecipeCheck effects => Eff effects ()
skills = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let campaign = \"skills\" :: CampaignLabel\nlet group = \"examples\" :: ForkGroupLabel\nlet work = Task (batch campaign group) \".exomonad/workspace/skills/exomonad-fork/SKILL.md\" " <> gitOidLiteral baseline <> " \"Exercise skill examples\" \"Check actual resident composition\" [] \"Typed result and progress\" []\nlet source = projectHead")
  void $ example owner "exomonad-fork" 0
  worker <- activation
  check "skill launches a fresh Luna Medium worker" (checkModel worker == Just "gpt-6-luna" && "Exercise skill examples" `Text.isInfixOf` checkContext worker)
  early <- turn owner "let Just group = forkGroupHandle worker\ncleanup <- releaseGroup group\ninspectFull cleanup"
  check "scoped release retains the pending worker instead of cancelling its request" ("CleanupBlocked" `Text.isInfixOf` lastOutput early)
  void $ example owner "exomonad-define-actors" 0
  joined <- turn owner "(== Just (\"abc123\",4)) <$> R.call (joined endpoints) ()"
  check "record skill joins differently typed inputs" (lastOutput joined == "True")
  initialResults <- example owner "exomonad-define-actors" 1
  check "record skill attaches the exact pending request" (lastOutput initialResults == "0")
  void $ turn (checkActor worker) ("let candidate = Candidate " <> gitOidLiteral baseline <> " [\"example check\"] [\"product acceptance remains\"]")
  void $ example (checkActor worker) "exomonad-coordinate" 0
  observed <- example owner "exomonad-coordinate" 1
  check "compact snapshot shows candidate and pending result" (baseline `Text.isInfixOf` output observed && "result pending" `Text.isInfixOf` output observed)
  void $ example (checkActor worker) "exomonad-review" 0
  reviewer <- activation
  void $ turn (checkActor reviewer) "let checks = [\"fixture review\"] :: [Text]\nlet scope = \"skill composition only\" :: Text"
  replied <- example (checkActor reviewer) "exomonad-review" 1
  check "successful reply explicitly reports submission" ("Reply submitted." `Text.isInfixOf` output replied)
  void $ turn (checkActor worker) "respond (Produced candidate)"
  final <- awaitOutput owner "state <- readWork router\ninspectFull (workSnapshotSummary candidateSummary state)" (not . Text.isInfixOf "result pending")
  check "compact snapshot retains terminal candidate and gates" (baseline `Text.isInfixOf` final && "product acceptance remains" `Text.isInfixOf` final)
  resultCount <- turn owner "R.call (resultCount (R.client results)) ()"
  check "record skill receives the typed terminal source once" (output resultCount == "1")
  void $ example owner "exomonad-define-actors" 2
  cleanup <- example owner "exomonad-coordinate" 2
  check "the coordinate skill retains its scoped cleanup receipt" ("CleanupReceipt" `Text.isInfixOf` lastOutput cleanup)

  -- The shipped decomposition example is a real multi-item cell. It uses an
  -- ordinary local selector, one inherited prefix and a selective collector.
  void $ turn owner ("let baseline = " <> gitOidLiteral baseline)
  void $ example owner "exomonad-coordinate" 3
  interface <- activation
  consumer <- activation
  let labels = map checkLabel [interface, consumer]
  check "one plan yields two distinct inherited Sol Medium assignments"
    (checkModel interface == Just "gpt-6-sol"
      && checkModel consumer == Just "gpt-6-sol"
      && "interface" `elem` labels && "consumer" `elem` labels
      && all (Text.isInfixOf "Deliver the feature through its real consumer" . checkContext) [interface, consumer])
  void $ example owner "exomonad-coordinate" 4
  void $ turn (checkActor interface) ("let found = Candidate " <> gitOidLiteral baseline <> " [\"interface evidence\"] []\nreportProgress (WorkProgress [found] [])")
  void $ turn (checkActor consumer) ("let found = Candidate " <> gitOidLiteral baseline <> " [\"consumer evidence\"] []\nreportProgress (WorkProgress [found] [])")
  observed <- awaitOutput owner "state <- readWork router\ninspectFull (map (workEvidence . sourceProgress) (collectedWork state))" (Text.isInfixOf "consumer evidence")
  check "collector retains both candidates without a routine wake" ("interface evidence" `Text.isInfixOf` observed && "consumer evidence" `Text.isInfixOf` observed)
  void $ turn (checkActor interface) "respond (Produced found)"
  void $ turn (checkActor consumer) "respond (Produced found)"
  void $ turn owner "drained <- finishWork router\nlet Just group = forkGroupHandle interface\nreleased <- releaseGroup group\ninspectFull released"

  -- The workbench skill's cells are the ones a model copies verbatim; each
  -- must typecheck and run with no surrounding context. Block 4 reads files
  -- and block 6 describes a command, so both need a command owner and are
  -- exercised by the exomonad-command material instead.
  void $ example owner "exomonad-workbench" 0
  converted <- example owner "exomonad-workbench" 1
  check "the workbench skill renders an Int into Text with T.pack . show"
    ("retry budget 3 exhausted" `Text.isInfixOf` output converted)
  annotated <- example owner "exomonad-workbench" 2
  check "an annotated polymorphic binding installs without being forced"
    ("annotated" `Text.isInfixOf` output annotated)
  void $ example owner "exomonad-workbench" 3
  void $ example owner "exomonad-workbench" 5

  -- The Jev skill's packets must compile against the real operators and
  -- resolve to a typed value whether or not an endpoint is configured. Block 0
  -- gathers previews with commands; the packet that judges them is block 1.
  void $ turn owner "let previews = [(\"README.md\", \"# jev-dsl\\ntyped packets\"), (\"LICENSE\", \"MIT\")] :: [(Text, Text)]"
  void $ example owner "exomonad-jev" 1
  gated <- example owner "exomonad-jev" 2
  check "the review gate resolves to an accepted key or a stated doubt"
    (any (`Text.isInfixOf` output gated) ["jev unavailable", "hold", "all_present", "one_absent", "contradicts", "insufficient_evidence"])
  continued <- example owner "exomonad-jev" 3
  check "the selected continuation is what runs, not a key string"
    (any (`Text.isInfixOf` output continued) ["jev unavailable", "would rerun", "reading the failure by hand"])
  void $ example owner "exomonad-jev" 4

  -- The unfold skill's launch and watch cells are the published doc examples;
  -- these two read a child's identity and its submission without touching the
  -- child's own checkout.
  void $ example owner "exomonad-unfold" 2
  observedSubmission <- example owner "exomonad-unfold" 3
  check "a settled child is inspected through its typed submission evidence"
    (not (Text.null (output observedSubmission)))

  -- Cleanup is inspected before it is executed; the plan is a value.
  planned <- example owner "exomonad-cleanup" 0
  check "the cleanup skill inspects a typed plan without retiring anything"
    ("Cleanup" `Text.isInfixOf` lastOutput planned)

  -- The project-free record definition: no Project.Actors wrapper, the row
  -- named in the cell and pinned by a signature on the definition.
  tallied <- example owner "exomonad-define-actors" 3
  check "a record definition pins its own effect row without a project wrapper"
    (lastOutput tallied == "1")

  -- The workbench cells a model copies to get past the run-6 rejections:
  -- where a signature goes, annotating a literal under ToJSON, and building
  -- the worktree identity types. Blocks 10 and 11 read a file and run a
  -- command, so they belong to the command material instead.
  placed <- example owner "exomonad-workbench" 7
  check "a signature beside its equation installs both forms of binding"
    ("high" `Text.isInfixOf` output placed && "lane 7" `Text.isInfixOf` output placed)
  annotatedLiteral <- example owner "exomonad-workbench" 8
  check "an annotated literal settles an otherwise ambiguous encodable field"
    ("src/app.rs" `Text.isInfixOf` output annotatedLiteral)
  identities <- example owner "exomonad-workbench" 9
  check "branch and ref identities round-trip through their constructors"
    ("integration/tags" `Text.isInfixOf` output identities
      && "exomonad/integration" `Text.isInfixOf` output identities)
