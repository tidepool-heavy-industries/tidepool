{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE TypeApplications #-}

-- | Select one existing deterministic production browser integration test.
-- The tests own authentication, loopback ports, temporary stores and WebSocket
-- behavior; this module supplies no browser transport.
module Project.BrowserScenario
  ( BrowserScenario (..)
  , BrowserWorkspace (..)
  , BrowserResources (..)
  , BrowserPreparation (..)
  , BrowserStartIssue (..)
  , BrowserWatchIssue (..)
  , BrowserReadyIssue (..)
  , BrowserStarted (..)
  , BrowserState (..)
  , BrowserActor (browserSnapshot)
  , startBrowserPreparation
  , watchBrowserScenarios
  , browserFocusedSpec
  , browserPreparationCommand
  , browserReadinessCommand
  ) where

import Control.Monad (forM)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)
import Project.CheckResults
  ( CheckActor, CheckSetupIssue, NoticePolicy (NotifyAllTerminal), watchChecks )
import Project.PrepareContinue (PreparationFailure, verifyPrepared)
import Project.TestEvidence
  ( FocusedRun, FocusedSetupIssue, FocusedSpec (..), startFocusedIn )
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Actor, Commands, Notifications)
import Tidepool.Effects.Row (knownEffects)

data BrowserScenario
  = BrowserJourney
  | StandaloneReopen
  deriving (Show, Eq)

data BrowserWorkspace = BrowserWorkspace
  { browserCheckout :: Text
  , browserSource :: Text
  } deriving (Show, Eq)

data BrowserResources = BrowserResources
  { preparationMemory :: Cmd.Memory
  , focusedMemory :: Cmd.Memory
  } deriving (Show, Eq)

-- | Bind this returned Job in the notebook before admitting the watcher. It
-- carries the exact checkout, source and resource policy to the next step.
data BrowserPreparation = BrowserPreparation
  { preparedWorkspace :: BrowserWorkspace
  , preparedResources :: BrowserResources
  , preparedJob :: Cmd.Job
  } deriving (Show)

data BrowserStartIssue
  = InvalidBrowserCheckout Text
  | BrowserPreparationRejected Cmd.CommandError
  deriving (Show, Eq)

data BrowserWatchIssue
  = NoBrowserScenarios
  | DuplicateBrowserScenario BrowserScenario
  deriving (Show, Eq)

data BrowserReadyIssue
  = BrowserReadinessStartFailed Cmd.CommandError
  | BrowserReadinessPending Cmd.Job Cmd.CommandStatus
  | BrowserReadinessFailed Cmd.RunResult
  deriving (Show)

-- | Every selected test is retained even if another test's setup is refused.
-- CheckResults owns terminal evidence and notices for every started run.
data BrowserStarted = BrowserStarted
  { browserRuns :: [(BrowserScenario, Either FocusedSetupIssue FocusedRun)]
  , browserChecks :: Maybe (Either CheckSetupIssue (ActorHandle CheckActor))
  } deriving (Show)

data BrowserState = BrowserState
  { browserPreparation :: Cmd.Job
  , browserResult :: Maybe (Either (PreparationFailure BrowserReadyIssue) BrowserStarted)
  , browserNotice :: Maybe (Either NotificationError NotificationReceipt)
  }

instance Show BrowserState where
  show state = "BrowserState { preparation = " ++ show (browserPreparation state)
    ++ ", result = " ++ (case browserResult state of
      Nothing -> "pending }"
      Just (Left _) -> "preparation failed }"
      Just (Right started) -> "focused setup " ++ show
        [(scenario, either (const "refused") (const "started") launched)
          | (scenario, launched) <- browserRuns started]
        ++ "; checks " ++ (case browserChecks started of
          Nothing -> "not started"
          Just (Left _) -> "refused"
          Just (Right _) -> "watching") ++ " }")

data BrowserActor mode = BrowserActor
  { browserState :: mode :- State BrowserState
  , browserSnapshot :: mode :- Call () (R.Reply BrowserState)
  , browserCompleted :: mode :- Event Cmd.CommandResult
  , browserAttachChecks :: mode :- Call () NoReply
  } deriving Generic

type BrowserEffects = R.LocalEffects BrowserActor '[Replies, Actor, Notifications, Commands]

-- | Start one source-guarded asset build; the returned preparation retains its
-- Job independently of later actor admission.
startBrowserPreparation
  :: Member Commands effects
  => BrowserWorkspace -> BrowserResources
  -> Eff effects (Either BrowserStartIssue BrowserPreparation)
startBrowserPreparation workspace resources
  | not ("/" `Text.isPrefixOf` browserCheckout workspace) =
      pure (Left (InvalidBrowserCheckout (browserCheckout workspace)))
  | otherwise = do
      started <- Cmd.tryBackground $ Cmd.withMemory (preparationMemory resources) $
        Cmd.inDirectory (browserCheckout workspace) (browserPreparationCommand workspace)
      pure $ case started of
        Left issue -> Left (BrowserPreparationRejected issue)
        Right job -> Right (BrowserPreparation workspace resources job)

-- | One preparation serves all selected scenarios. The actor subscribes to
-- its exact completion, starts each focused test in the supplied checkout,
-- and gives CheckResults their terminal evidence.
watchBrowserScenarios
  :: Member Actor effects
  => AgentRef -> BrowserPreparation -> [BrowserScenario]
  -> Eff effects (Either BrowserWatchIssue (ActorHandle BrowserActor))
watchBrowserScenarios owner preparation scenarios
  | null scenarios = pure (Left NoBrowserScenarios)
  | Just duplicate <- firstDuplicate scenarios = pure (Left (DuplicateBrowserScenario duplicate))
  | otherwise = Right <$> R.start (browserDefinition owner preparation scenarios)

firstDuplicate :: [BrowserScenario] -> Maybe BrowserScenario
firstDuplicate [] = Nothing
firstDuplicate (scenario : rest)
  | scenario `elem` rest = Just scenario
  | otherwise = firstDuplicate rest

browserDefinition
  :: AgentRef -> BrowserPreparation -> [BrowserScenario]
  -> ActorSpec BrowserActor BrowserEffects
browserDefinition owner preparation scenarios =
  R.definition "browser-scenarios" (Actor.Selected knownEffects) BrowserActor
    { browserState = BrowserState job Nothing Nothing
    , browserSnapshot = \() -> R.get
    , browserCompleted = R.on (Cmd.completion job) $ \completion -> do
        result <- verifyPrepared job completion $ \_ -> do
          readiness <- checkBrowserReadiness workspace
          case readiness of
            Left issue -> pure (Left issue)
            Right () -> Right <$> startFocusedScenarios workspace resources scenarios
        R.modify' (\state -> state { browserResult = Just result })
        case result of
          Right _ -> do
            own <- R.self @BrowserActor
            R.send (browserAttachChecks own) ()
          Left _ -> pure ()
        notice <- case result of
          Left _ -> Just <$> sendMessage owner
            ("Browser preparation failed; inspect retained preparation job "
              <> Text.pack (show job) <> " and browserSnapshot")
          Right started | setupIncomplete started ->
              Just <$> sendMessage owner
                ("Browser test setup incomplete; inspect browserSnapshot and retained runs")
          _ -> pure Nothing
        R.modify' (\state -> state { browserResult = Just result, browserNotice = notice })
    , browserAttachChecks = \() -> do
        current <- R.get
        case browserResult current of
          Just (Right started) | Nothing <- browserChecks started -> do
            let focused = [(Text.pack (show scenario), run)
                  | (scenario, Right run) <- browserRuns started]
            watcher <- case focused of
              [] -> pure Nothing
              _ -> Just <$> watchChecks owner NotifyAllTerminal focused
            R.modify' (\state -> state { browserResult = Just (Right
              started { browserChecks = watcher }) })
          _ -> pure ()
    }
  where
    workspace = preparedWorkspace preparation
    resources = preparedResources preparation
    job = preparedJob preparation

setupIncomplete :: BrowserStarted -> Bool
setupIncomplete started =
  any (either (const True) (const False) . snd) (browserRuns started)
    || case browserChecks started of
      Just (Left _) -> True
      _ -> False

checkBrowserReadiness
  :: Member Commands effects
  => BrowserWorkspace -> Eff effects (Either BrowserReadyIssue ())
checkBrowserReadiness workspace = do
  started <- Cmd.tryStart $ Cmd.inDirectory (browserCheckout workspace)
    (browserReadinessCommand workspace)
  case started of
    Left issue -> pure (Left (BrowserReadinessStartFailed issue))
    Right job -> do
      status <- Cmd.quiet (Cmd.observe (Cmd.Observation 30000 0) job)
      case status of
        Cmd.CommandFinished _ -> do
          receipt <- Cmd.quiet (Cmd.await job)
          pure $ if Cmd.failure receipt == Nothing
              && Cmd.commandCleanup (Cmd.commandResult receipt) == Cmd.CommandClean
            then Right ()
            else Left (BrowserReadinessFailed receipt)
        other -> pure (Left (BrowserReadinessPending job other))

startFocusedScenarios
  :: Member Commands effects
  => BrowserWorkspace -> BrowserResources -> [BrowserScenario]
  -> Eff effects BrowserStarted
startFocusedScenarios workspace resources scenarios = do
  runs <- forM scenarios $ \scenario -> do
    started <- startFocusedIn (browserCheckout workspace) (focusedMemory resources)
      (browserFocusedSpec workspace scenario)
    pure (scenario, started)
  pure (BrowserStarted runs Nothing)

-- | The web prerequisites from `scripts/verify-browser-journey`, ending
-- before that script's browser assertion. Source and clean-tree checks bracket
-- the build in the actor's checkout; the caller retains this command's job.
browserPreparationCommand :: BrowserWorkspace -> Cmd.Command
browserPreparationCommand workspace = Cmd.argv
  [ "nix", "develop", ".#web", "-c", "bash", "-lc"
  , "set -euo pipefail; test \"$(git rev-parse HEAD)\" = \"$1\"; test -z \"$(git status --porcelain)\"; cd web; npm ci; npm run check; npm test; npm run build; cd ..; test \"$(git rev-parse HEAD)\" = \"$1\"; test -z \"$(git status --porcelain)\"; test -f web/dist/index.html"
  , "browser-prepare", browserSource workspace
  ]

-- | A second read in the same checkout after successful preparation. Its
-- completed result confirms an asset exists; it is not a freshness witness
-- and never authorizes skipping the preparation command.
browserReadinessCommand :: BrowserWorkspace -> Cmd.Command
browserReadinessCommand workspace = Cmd.argv
  [ "bash", "-lc"
  , "set -euo pipefail; test \"$(git rev-parse HEAD)\" = \"$1\"; test -z \"$(git status --porcelain)\"; test -f web/dist/index.html"
  , "browser-ready", browserSource workspace
  ]

browserFocusedSpec :: BrowserWorkspace -> BrowserScenario -> FocusedSpec
browserFocusedSpec workspace scenario = case scenario of
  BrowserJourney -> FocusedSpec
    { focusedIntent = "authenticated browser command, pending/cancel, child failure and reconnect"
    , focusedSource = browserSource workspace
    , focusedPackage = "harness-demo"
    , focusedTarget = "test:browser_journey"
    , focusedFilter = "browser_journey_auth_pending_cancel_child_failure_and_reconnect"
    , focusedExpected = 1
    }
  StandaloneReopen -> FocusedSpec
    { focusedIntent = "standalone browser asset, clean restart and process-loss reopen"
    , focusedSource = browserSource workspace
    , focusedPackage = "harness-demo"
    , focusedTarget = "test:standalone_browser"
    , focusedFilter = "standalone_missing_assets_and_clean_and_process_loss_reopen"
    , focusedExpected = 1
    }
