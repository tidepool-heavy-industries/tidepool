{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.SkillChecks (skills) where

import Prelude hiding (readFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Check
import Tidepool.Aeson (Value)

-- Execute the skill's actual code blocks, not separately maintained copies.
example :: Member RecipeCheck effects => CheckActor -> Text -> Int -> Eff effects Value
example actor skill index = do
  body <- readFile actor (".shoal/skills/" <> skill <> "/SKILL.md")
  let blocks = map (fst . Text.breakOn "```") (drop 1 (Text.splitOn "```haskell\n" body))
  turn actor (blocks !! index)

skills :: Member RecipeCheck effects => Eff effects ()
skills = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let Right campaign = campaignLabel \"skills\"\nlet Right group = forkGroupLabel \"examples\"\nlet task = Task (batch campaign group) \".shoal/skills/shoal-fork/SKILL.md\" " <> literal baseline <> " \"Exercise skill examples\" \"Check actual resident composition\" [] \"Typed result and progress\" []\nlet source = projectHead")
  void $ example owner "shoal-fork" 0
  worker <- activation
  check "skill launches a fresh Sol Medium worker" (checkModel worker == Just "gpt-5.6-sol" && "Exercise skill examples" `Text.isInfixOf` checkContext worker)
  early <- turn owner "cleanup <- releaseGroup (forkGroupHandle worker)\ninspectFull cleanup"
  check "scoped release retains the pending worker instead of cancelling its request" ("CleanupBlocked" `Text.isInfixOf` lastOutput early)
  void $ example owner "shoal-define-actors" 0
  joined <- turn owner "(== Just (\"abc123\",4)) <$> R.call (joined endpoints) ()"
  check "record skill joins differently typed inputs" (lastOutput joined == "True")
  initialResults <- example owner "shoal-define-actors" 1
  check "record skill attaches the exact pending request" (lastOutput initialResults == "0")
  void $ turn (checkActor worker) ("let candidate = Candidate " <> literal baseline <> " [\"example check\"] [\"product acceptance remains\"]")
  void $ example (checkActor worker) "shoal-coordinate" 0
  observed <- example owner "shoal-coordinate" 1
  check "compact snapshot shows candidate and pending result" (baseline `Text.isInfixOf` output observed && "result pending" `Text.isInfixOf` output observed)
  void $ example (checkActor worker) "shoal-review" 0
  reviewer <- activation
  void $ turn (checkActor reviewer) "let checks = [\"fixture review\"] :: [Text]\nlet scope = \"skill composition only\" :: Text"
  replied <- example (checkActor reviewer) "shoal-review" 1
  check "successful reply explicitly reports submission" ("Reply submitted." `Text.isInfixOf` output replied)
  void $ turn (checkActor worker) "respond (Produced candidate)"
  final <- awaitOutput owner "state <- readWork router\ninspectFull (workSnapshotSummary candidateSummary state)" (not . Text.isInfixOf "result pending")
  check "compact snapshot retains terminal candidate and gates" (baseline `Text.isInfixOf` final && "product acceptance remains" `Text.isInfixOf` final)
  resultCount <- turn owner "R.call (resultCount (R.client results)) ()"
  check "record skill receives the typed terminal source once" (output resultCount == "1")
  void $ example owner "shoal-define-actors" 2
  cleanup <- example owner "shoal-coordinate" 2
  check "the coordinate skill retains its scoped cleanup receipt" ("CleanupReceipt" `Text.isInfixOf` lastOutput cleanup)
