{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Prepare an exact source and await the selected deterministic production
-- browser integration tests in one invocation. The tests own browser transport.
module Project.BrowserScenario
  ( BrowserScenario (..)
  , BrowserWorkspace (..)
  , BrowserResources (..)
  , BrowserRunIssue (..)
  , BrowserResult (..)
  , runBrowserScenarios
  , browserFocusedSpec
  , browserPreparationCommand
  , browserReadinessCommand
  ) where

import Control.Monad (forM)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Project.TestEvidence
  ( FocusedResult, FocusedSetupIssue, FocusedSpec (..)
  , collectFocused, startFocusedScopedIn )
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands)

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

data BrowserRunIssue
  = InvalidBrowserCheckout Text
  | NoBrowserScenarios
  | DuplicateBrowserScenario BrowserScenario
  | BrowserPreparationRejected Cmd.CommandError
  | BrowserPreparationFailed Cmd.RunResult
  | BrowserReadinessRejected Cmd.RunResult Cmd.CommandError
  | BrowserReadinessFailed Cmd.RunResult Cmd.RunResult
  deriving (Show)

-- | Receipts retain exact jobs and independent process, cleanup and output
-- facts. Every selected scenario remains present even if another start fails.
data BrowserResult = BrowserResult
  { browserPreparation :: Cmd.RunResult
  , browserReadiness :: Cmd.RunResult
  , browserResults :: [(BrowserScenario, Either FocusedSetupIssue FocusedResult)]
  } deriving (Show)

runBrowserScenarios
  :: Member Commands effects
  => BrowserWorkspace -> BrowserResources -> [BrowserScenario]
  -> Eff effects (Either BrowserRunIssue BrowserResult)
runBrowserScenarios workspace resources scenarios
  | not ("/" `Text.isPrefixOf` browserCheckout workspace) =
      pure (Left (InvalidBrowserCheckout (browserCheckout workspace)))
  | null scenarios = pure (Left NoBrowserScenarios)
  | Just duplicate <- firstDuplicate scenarios =
      pure (Left (DuplicateBrowserScenario duplicate))
  | otherwise = do
      preparationStart <- Cmd.tryStart $ Cmd.withMemory (preparationMemory resources) $
        Cmd.inDirectory (browserCheckout workspace) (browserPreparationCommand workspace)
      case preparationStart of
        Left issue -> pure (Left (BrowserPreparationRejected issue))
        Right preparationJob -> do
          preparation <- Cmd.await preparationJob
          if not (prerequisitePassed preparation)
            then pure (Left (BrowserPreparationFailed preparation))
            else do
              readinessStart <- Cmd.tryStart $ Cmd.withMemory (preparationMemory resources) $
                Cmd.inDirectory (browserCheckout workspace) (browserReadinessCommand workspace)
              case readinessStart of
                Left issue -> pure (Left (BrowserReadinessRejected preparation issue))
                Right readinessJob -> do
                  readiness <- Cmd.await readinessJob
                  if not (prerequisitePassed readiness)
                    then pure (Left (BrowserReadinessFailed preparation readiness))
                    else do
                      started <- forM scenarios $ \scenario -> do
                        run <- startFocusedScopedIn (browserCheckout workspace) (focusedMemory resources)
                          (browserFocusedSpec workspace scenario)
                        pure (scenario, run)
                      results <- forM started $ \(scenario, run) -> do
                        result <- case run of
                          Left issue -> pure (Left issue)
                          Right focused -> Right <$> collectFocused focused
                        pure (scenario, result)
                      pure (Right (BrowserResult preparation readiness results))

prerequisitePassed :: Cmd.RunResult -> Bool
prerequisitePassed receipt = case
  (Cmd.commandOutcome (Cmd.commandResult receipt), Cmd.commandCleanup (Cmd.commandResult receipt)) of
    (Cmd.CommandExited 0, Cmd.CommandClean) -> True
    _ -> False

firstDuplicate :: [BrowserScenario] -> Maybe BrowserScenario
firstDuplicate [] = Nothing
firstDuplicate (scenario : rest)
  | scenario `elem` rest = Just scenario
  | otherwise = firstDuplicate rest

-- | The web prerequisites from `scripts/verify-browser-journey`, ending
-- before that script's browser assertion. Source and clean-tree checks bracket
-- the build in the supplied checkout; the continuation retains the terminal receipt.
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
