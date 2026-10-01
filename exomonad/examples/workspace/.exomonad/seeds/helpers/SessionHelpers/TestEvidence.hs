{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Remix this session seed for the current component. Keep evidence parsing
-- and acceptance rules in the shared owner; specialize commands and policy here.
module SessionHelpers.TestEvidence
  ( module Project.TestEvidence, runTests, CheckDefinition (..), checkAt, runCheck
  , module Exomonad.Contrib.CheckPlan, plannedCheck
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Exomonad.Contrib.CheckPlan
import Project.TestEvidence
import qualified Tidepool.Command as Cmd
import Tidepool.Actors.Exomonad (AgentRef)
import Tidepool.Effects.Core (Actor, Commands)
import Tidepool.Worktree (GitOid, renderGitOid)

-- Remix the definition; supply the committed candidate at each invocation.
data CheckDefinition = CheckDefinition
  { checkIntent :: Text, checkPackage :: Text, checkTarget :: Text
  , checkFilter :: Text, checkExpected :: Int
  } deriving (Show, Eq)

checkAt :: GitOid -> CheckDefinition -> FocusedSpec
checkAt candidate definition = FocusedSpec
  (checkIntent definition) (renderGitOid candidate) (checkPackage definition)
  (checkTarget definition) (checkFilter definition) (checkExpected definition)

runCheck :: (Member Actor effects, Member Commands effects) => AgentRef -> GitOid -> Cmd.Memory -> CheckDefinition -> Eff effects GateStart
runCheck owner candidate memory definition =
  startGate ["scripts/cargo-focused-test"] owner (checkIntent definition) memory (checkAt candidate definition)

-- | Adapt one reusable project definition to the shared check plan.
plannedCheck :: CheckDefinition -> Cmd.Memory -> CheckPreparation -> PlanCheck
plannedCheck definition memory preparation = PlanCheck
  { planName = checkIntent definition
  , planRunner = ["scripts/cargo-focused-test"]
  , planSpec = (`checkAt` definition)
  , planMemory = memory
  , planPreparation = preparation
  }

-- Start once, retain the result, then compose watchChecks or collectFocused.
-- Tune this reservation and specialize a FocusedSpec for the current work.
runTests
  :: Member Commands effects
  => FocusedSpec -> Eff effects (Either FocusedSetupIssue FocusedRun)
runTests = startFocused (Cmd.GiB 4)
