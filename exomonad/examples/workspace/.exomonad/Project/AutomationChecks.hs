{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Exercise the combined helper surface in the shipped notebook namespace.
module Project.AutomationChecks (integration) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check
import Exomonad.Contrib.CheckResults (CheckState (..))
import Project.HandoffExamples (handoffProposal)
import Exomonad.Contrib.Types (Outcome (..))
import qualified Project.AssumptionChecks as Assumption
import qualified Project.AutomationRuntimeChecks as Commands
import qualified Project.InterviewChecks as Interview
import qualified Project.PrepareContinueChecks as Preparation
import qualified Project.RoutingChecks as Routing

integration :: Member RecipeCheck effects => Eff effects ()
integration = do
  let proposal = handoffProposal (Blocked "no candidate" []) Nothing (CheckState [] [] Nothing) ["run the selected tests"]
  check "text: handoff keeps missing review and remaining checks explicit"
    (all (`Text.isInfixOf` proposal)
      ["Reported candidate:", "Reported review: not supplied", "run the selected tests"])
  Assumption.changes
  void restart
  Interview.collectAnswers
  void restart
  Routing.reviewReadiness
  void restart
  Preparation.preparationCompletion
  void restart
  Commands.commandCustody
