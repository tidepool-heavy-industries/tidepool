{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

module Project.PrepareContinueChecks
  (preparationCompletion, PrepareActor, watchPreparation, readPreparation, finishPreparation) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad
import qualified Tidepool.Command as Cmd
import Tidepool.Check
import Tidepool.Effects.Core (Actor, Commands)
import Tidepool.Effects.Row (knownEffects)
import Project.PrepareContinue

data PrepareActor mode = PrepareActor
  { prepareState :: mode :- State (Maybe (Either (PreparationFailure Text) Text))
  , prepareSnapshot :: mode :- Call () (R.Reply (Maybe (Either (PreparationFailure Text) Text)))
  , prepareCompletion :: mode :- Event Cmd.CommandResult
  } deriving Generic

type PrepareEffects = LocalEffects PrepareActor '[Replies, Actor, Commands]

watchPreparation :: Member Actor effects => Cmd.Job -> Bool -> Eff effects (ActorHandle PrepareActor)
watchPreparation job ready = R.start (prepareDefinition job ready)

prepareDefinition :: Cmd.Job -> Bool -> ActorSpec PrepareActor PrepareEffects
prepareDefinition job ready =
  R.definition "prepared-command-check" (Actor.Selected knownEffects) PrepareActor
    { prepareState = Nothing
    , prepareSnapshot = \() -> R.get
    , prepareCompletion = R.on (Cmd.completion job) $ \receipt -> do
        verified <- verifyPrepared job receipt $ \_ ->
          pure (if ready then Right "ready" else Left "prerequisite missing")
        R.put (Just verified)
    }

readPreparation :: Member Actor effects => ActorHandle PrepareActor -> Eff effects (Maybe (Either (PreparationFailure Text) Text))
readPreparation actor = R.call (prepareSnapshot (R.client actor)) ()

finishPreparation :: Member Actor effects => ActorHandle PrepareActor -> Eff effects (Actor.ActorExit (Maybe (Either (PreparationFailure Text) Text)))
finishPreparation = R.finish

preparationCompletion :: Member RecipeCheck effects => Eff effects ()
preparationCompletion = do
  owner <- root
  void $ turn owner "import Project.PrepareContinueChecks"
  void $ turn owner $ Text.unlines
    [ "okJob <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"echo ready\"]))"
    , "badJob <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"printf failure-diagnostic >&2; exit 7\"]))"
    ]
  void $ awaitOutput owner "Cmd.status okJob" (Text.isInfixOf "CommandFinished")
  void $ turn owner "okWatcher <- watchPreparation okJob True"
  success <- awaitOutput owner
    "readPreparation okWatcher"
    (Text.isInfixOf "Just (Right \"ready\")")
  check "late preparation completion resumes with typed readiness" ("Just (Right \"ready\")" `Text.isInfixOf` success)
  void $ turn owner "finishPreparation okWatcher"

  void $ turn owner "badWatcher <- watchPreparation badJob True"
  failed <- awaitOutput owner
    "readPreparation badWatcher"
    (Text.isInfixOf "PreparationCommandFailed")
  check "failed preparation retains original command receipt" ("CommandExited 7" `Text.isInfixOf` failed)
  void $ turn owner "finishPreparation badWatcher"

  budget <- turn owner "evidenceBudget 0"
  check "retained read refuses zero byte budget" ("InvalidEvidenceBudget 0" `Text.isInfixOf` lastOutput budget)
  recovered <- turn owner
    "Right allowance <- pure (evidenceBudget 64)\nrecoverRetained allowance badJob"
  check "bounded pages recover the failed job without resubmission"
    (all (`Text.isInfixOf` lastOutput recovered)
      ["CommandExited 7", "failure-diagnostic", "StreamComplete"])
  bounded <- turn owner $ Text.unlines
    [ "largeResult <- Cmd.quiet (Cmd.run (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"printf abcdefghij\"])))"
    , "Right tiny <- pure (evidenceBudget 4)"
    , "small <- recoverRetained tiny (Cmd.job largeResult)"
    , "(streamStop (retainedStdout small), map Cmd.pageText (streamPages (retainedStdout small)))"
    ]
  check "retained page reading stops at the requested byte budget"
    (all (`Text.isInfixOf` lastOutput bounded) ["StreamBudgetReached", "abcd"])

  void $ turn owner "missingWatcher <- watchPreparation okJob False"
  missing <- awaitOutput owner
    "readPreparation missingWatcher"
    (Text.isInfixOf "PreparationReadinessFailed")
  check "readiness failure stays separate from successful command"
    (all (`Text.isInfixOf` missing) ["prerequisite missing", "CommandExited 0"])
  void $ turn owner "finishPreparation missingWatcher"
