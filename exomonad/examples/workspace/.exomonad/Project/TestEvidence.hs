{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Project policy for running focused Cargo checks in this repository.
module Project.TestEvidence
  ( module Exomonad.Contrib.Check.Cargo
  , startFocused, startFocusedIn, startFocusedScopedIn, startFocusedAfter
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Exomonad.Contrib.Check.Cargo
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands)

startFocused :: Member Commands effects => Cmd.Memory -> FocusedSpec -> Eff effects (Either FocusedSetupIssue FocusedRun)
startFocused = startFocusedWith ["scripts/cargo-focused-test"]

startFocusedIn :: Member Commands effects => Text -> Cmd.Memory -> FocusedSpec -> Eff effects (Either FocusedSetupIssue FocusedRun)
startFocusedIn = startFocusedInWith ["scripts/cargo-focused-test"]

startFocusedAfter :: Member Commands effects => Cmd.Memory -> FocusedSpec -> [Text] -> Eff effects (Either FocusedSetupIssue FocusedRun)
startFocusedAfter = startFocusedAfterWith ["scripts/cargo-focused-test"]

-- | Invocation-owned focused work; collect its terminal evidence in this cell.
startFocusedScopedIn :: Member Commands effects => Text -> Cmd.Memory -> FocusedSpec -> Eff effects (Either FocusedSetupIssue FocusedRun)
startFocusedScopedIn = startFocusedScopedInWith ["scripts/cargo-focused-test"]
