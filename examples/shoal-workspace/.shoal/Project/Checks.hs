{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}

-- Executable examples, run against this candidate package without any models.
-- The driver does not know the project roles or order: this ordinary Haskell does.
module Project.Checks (workbench, context, script, checkImprovement) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Check

script :: Member RecipeCheck effects => CheckActor -> Text -> Eff effects ()
script actor name = readFile actor (".shoal/checks/" <> name <> ".hs") >>= void . turn actor

context :: Member RecipeCheck effects => Eff effects ()
context = void startComponent

startComponent :: Member RecipeCheck effects => Eff effects (CheckActor, Activation)
startComponent = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let baseline = " <> gitOidLiteral baseline)
  script owner "component-setup"
  lead <- activation
  check "fresh Sol lead receives the engineering rationale" (checkModel lead == Just "gpt-5.6-sol" && "Display edges must not fabricate authority or lose actors." `Text.isInfixOf` checkContext lead)
  pure (owner, lead)

workbench :: Member RecipeCheck effects => Eff effects ()
workbench = do
  (owner, lead) <- startComponent
  script owner "observation-projection"
  projection <- turn owner "inspectFull (map rosterActorId (snapshotActors focusedActors) == [1,2,3,5,6,8,9] && length (actorSummary focusedActors) == 7)"
  check "compact roster retains work and uncertain terminals" (output projection == "True")
  candidate <- checkpoint (checkActor lead) "feature.txt" "candidate feature\n" "implement feature"
  void $ turn (checkActor lead) ("let candidate = Candidate " <> gitOidLiteral candidate <> " [\"implementation content check\"] [\"open product gate\"]")
  script (checkActor lead) "component-review"
  reviewer <- activation
  check "review receives the exact candidate and task" (candidate `Text.isInfixOf` checkContext reviewer && "projection/README.md" `Text.isInfixOf` checkContext reviewer)
  script (checkActor reviewer) "owner-repair"
  pending <- turn (checkActor lead) "import Tidepool.Agent.Reply (pollReply)\npollReply sessionReply"
  check "a failed review preserves the lead's delivery" (output pending == "ReplyOpen")
  revised <- checkpoint (checkActor lead) "feature.txt" "repaired feature\n" "repair feature"
  void $ turn (checkActor lead) ("let revised = Candidate " <> gitOidLiteral revised <> " [\"repair content check\"] [\"open product gate\"]")
  script (checkActor lead) "review-again"
  reused <- activation
  revisedInput <- turn (checkActor reused) "inspectFull (reviewInput sessionInput)"
  check "the same reviewer receives the repaired revision" (checkActor reused == checkActor reviewer && revised `Text.isInfixOf` output revisedInput)
  void $ git (checkActor reviewer) ["merge", "--ff-only", revised]
  content <- readFile (checkActor reviewer) "feature.txt"
  check "review checks the repaired source" (content == "repaired feature\n")
  void $ turn (checkActor reviewer) "respond (Produced (Accepted (ReviewedCandidate (reviewAssignment sessionInput) (reviewInput sessionInput) [\"review repair content check\"] \"coherent preparation\")))"
  script (checkActor lead) "deliver"
  delivered <- turn owner "delivered <- pollResponse lead\ninspectFull delivered"
  check "partial delivery retains the repaired head and gate" (revised `Text.isInfixOf` output delivered && "open product gate" `Text.isInfixOf` output delivered && not (candidate `Text.isInfixOf` output delivered))
  void $ git owner ["merge", "--ff-only", revised]
  combined <- readFile owner "feature.txt"
  check "application owner incorporates and checks delivery" (combined == "repaired feature\n")
  script owner "rsi"
  checkImprovement owner

-- The caller commissions RSI with the evidence from its own completed work.
checkImprovement :: Member RecipeCheck effects => CheckActor -> Eff effects ()
checkImprovement owner = do
  improver <- activation
  check "requested RSI is an ordinary selected Astra" (checkModel improver == Just "gpt-6-astra" && "Current definitions:" `Text.isInfixOf` checkContext improver)
  prompt <- readFile (checkActor improver) ".shoal/prompts/task.md"
  next <- checkpoint (checkActor improver) ".shoal/prompts/task.md" (prompt <> "\nRecipe improvement: carry the checked contract.\n") "improve next-wave task guidance"
  void $ turn (checkActor improver) ("respond (Produced (Candidate " <> gitOidLiteral next <> " [\"authored guidance check\"] [\"activate next swarm\"]))")
  void $ git owner ["merge", "--ff-only", next]
  frozen <- turn owner "inspectFull (fmap (T.isInfixOf \"Recipe improvement\") (workspacePrompt \"task\"))"
  check "in-flight definitions remain frozen" (output frozen == "Just False")
  void restart
  nextOwner <- root
  selected <- turn nextOwner "inspectFull (fmap (T.isInfixOf \"Recipe improvement\") (workspacePrompt \"task\"))"
  check "the explicit next swarm consumes the changed prompt" (output selected == "Just True")
