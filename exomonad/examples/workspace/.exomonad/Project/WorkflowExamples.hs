{-# LANGUAGE FlexibleContexts #-}

-- | Remixable compositions of the existing owners. Readiness is project code:
-- it can verify the source and assets and return the check appropriate to them.
module Project.WorkflowExamples (PreparedCheckIssue (..), prepareFocused) where

import Control.Monad.Freer (Eff, Member)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands)
import Project.PrepareContinue
import Project.TestEvidence

data PreparedCheckIssue issue
  = PrerequisiteIssue (PreparationFailure issue)
  | CheckStartIssue FocusedSetupIssue
  deriving (Show)

-- | Invoke in the preparation job's completion handler. The readiness callback
-- runs with that handler's authority; a path alone grants no checkout access.
-- Starting the check returns its original handle for CheckResults.watchChecks.
prepareFocused
  :: Member Commands effects
  => Cmd.Memory
  -> Cmd.Job
  -> Cmd.CommandResult
  -> (Cmd.RunResult -> Eff effects (Either issue FocusedSpec))
  -> Eff effects (Either (PreparedCheckIssue issue) FocusedRun)
prepareFocused memory preparation receipt readiness = do
  ready <- verifyPrepared preparation receipt readiness
  case ready of
    Left issue -> pure (Left (PrerequisiteIssue issue))
    Right spec -> fmap (either (Left . CheckStartIssue) Right) (startFocused memory spec)
