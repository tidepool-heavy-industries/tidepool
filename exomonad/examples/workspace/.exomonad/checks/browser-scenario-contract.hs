{-# LANGUAGE OverloadedStrings #-}

module Main (main) where

import Control.Monad (unless)
import qualified Data.Text as Text
import Project.BrowserScenario
import Project.TestEvidence (FocusedSpec (..))
import qualified Tidepool.Command as Cmd

main :: IO ()
main = do
  let source = "0123456789abcdef0123456789abcdef01234567"
      workspace = BrowserWorkspace "/tmp/harness-checkout" source
      journey = browserFocusedSpec workspace BrowserJourney
      standalone = browserFocusedSpec workspace StandaloneReopen
  unless (focusedSource journey == source && focusedSource standalone == source)
    (error "scenario lost the exact source")
  unless (focusedTarget journey == "test:browser_journey"
      && focusedFilter journey == "browser_journey_auth_pending_cancel_child_failure_and_reconnect"
      && focusedExpected journey == 1)
    (error "journey does not select exactly the production browser test")
  unless (focusedTarget standalone == "test:standalone_browser"
      && focusedFilter standalone == "standalone_missing_assets_and_clean_and_process_loss_reopen"
      && focusedExpected standalone == 1)
    (error "standalone does not select exactly the reconnect and reopen test")
  let preparation = Cmd.commandArgv (Cmd.describe (browserPreparationCommand workspace))
  unless (length preparation == 9
      && take 6 preparation == ["nix", "develop", ".#web", "-c", "bash", "-lc"]
      && preparation !! 7 == "browser-prepare"
      && preparation !! 8 == source
      && all (`Text.isInfixOf` (preparation !! 6))
        ["npm ci", "npm run check", "npm test", "npm run build"]
      && Text.count "git rev-parse HEAD" (preparation !! 6) == 2
      && Text.count "git status --porcelain" (preparation !! 6) == 2)
    (error "browser preparation lost its source or required web steps")
  let readiness = Cmd.commandArgv (Cmd.describe (browserReadinessCommand workspace))
  unless (length readiness == 4
      && take 2 readiness == ["bash", "-lc"]
      && readiness !! 3 == source
      && all (`Text.isInfixOf` (readiness !! 2))
        ["git rev-parse HEAD", "git status --porcelain", "test -f web/dist/index.html"])
    (error "browser readiness must inspect the prepared asset")
