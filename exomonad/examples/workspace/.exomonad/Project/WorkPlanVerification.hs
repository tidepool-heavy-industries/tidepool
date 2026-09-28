{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | Run product checks in a fresh managed checkout of the exact published
-- revision. The coordinator retains the returned actor until the check
-- watcher settles, then finishes it; command and watcher receipts remain in
-- the returned 'PlanStart'.
module Project.WorkPlanVerification
  ( VerificationRunner
  , startExactVerification
  ) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.Char (isAsciiLower, isDigit)
import Data.String (fromString)
import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad
  ( (:-), Actor, WorktreeAllocation, GitOid, createWorktree
  , fromRef, worktreeId, renderGitOid )
import Tidepool.Effects.Core (Commands)
import Tidepool.Effects.Row (knownEffects)
import Tidepool.Worktree (renderWorktreeError)
import Project.CheckResults (CheckSetupIssue, CheckState)
import Project.FocusedGateExample
  ( PlanCheck, PlanStart (..), PlanReport (..), planSummary, startCheckPlanInto )

data VerificationRunner mode = VerificationRunner
  { runnerState :: mode :- R.State ()
  , runnerLaunch :: mode :- R.Call () (R.Reply (Either CheckSetupIssue PlanStart))
  } deriving Generic

type VerificationEffects = R.LocalEffects VerificationRunner '[Actor, Commands]

runner
  :: GitOid -> [PlanCheck] -> R.Send CheckState
  -> R.ActorSpec VerificationRunner VerificationEffects
runner checked checks destination =
  R.definition "work-plan-verification" (Actor.Selected knownEffects) VerificationRunner
    { runnerState = ()
    , runnerLaunch = \() -> startCheckPlanInto destination checked checks
    }

-- | The namespace is the coordinator's full worktree id; a ticket is unique
-- inside that coordinator. Reject lossy name normalization, so distinct
-- coordinators cannot silently select the same branch. Worktree allocation
-- refuses any preexisting branch and never falls back to the coordinator's
-- checkout.
startExactVerification
  :: (Member WorktreeAllocation effects, Member Actor effects)
  => GitOid -> Text -> Int -> [PlanCheck] -> R.Send CheckState
  -> Eff effects (Either Text (PlanStart, R.ActorHandle VerificationRunner))
startExactVerification checked namespace ticket checks destination
  | Text.null namespace || not (Text.all safe namespace) =
      pure (Left "verification namespace must be a nonempty worktree id using lowercase letters, digits, and hyphens")
  | ticket < 0 = pure (Left "verification ticket must be nonnegative")
  | otherwise = do
      let name = "work-plan-verify-" <> namespace <> "-" <> Text.pack (show ticket)
      allocated <- createWorktree (fromRef (fromString (Text.unpack (renderGitOid checked))) name)
      case allocated of
        Left failure -> pure (Left ("verification checkout allocation refused: " <> renderWorktreeError failure))
        Right tree -> do
          running <- R.start (R.withWorktree (worktreeId tree) (runner checked checks destination))
          started <- R.call (runnerLaunch (R.client running)) ()
          case started of
            Left issue -> do
              void (R.finish running)
              pure (Left ("verification check plan refused: " <> Text.pack (show issue)))
            Right planStart -> case planWatcher planStart of
              Just (Right _) -> pure (Right (planStart, running))
              _ -> do
                let summary = planSummary (PlanReport planStart Nothing)
                void (R.finish running)
                pure (Left ("verification watcher unavailable: " <> summary))
  where
    safe char = isAsciiLower char || isDigit char || char == '-'
