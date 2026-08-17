{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for S1-L1 outer-row servicing: a harness whose 'loop' calls
-- Console (@say@), Worktree (@createWorktree@), Exec (@run@), a
-- RepoEvent @withHandler@\/@headChanged@ subscribe-drain-unsubscribe cycle,
-- and Journal (@record@) DIRECTLY (no model round at all — the loop is
-- authored orchestration), so the driver's
-- Console\/Worktree\/RepoEvent\/Exec\/Journal suspension-servicing paths are
-- the only thing under test.
module OuterEffectsHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

import Tidepool.Effects
  ( createWorktree
  , fromCurrentRepository
  , headChanged
  , record
  , renderWorktreeError
  , run
  , say
  , withHandler
  )
import Tidepool.Harness (Harness)

data State = State
  { runs :: Int
  , lastError :: Text
  , execOutput :: Text
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {runs = 0, lastError = "", execOutput = ""}

render :: State -> Text
render st =
  [fmt|Outer-effects harness. Runs: {runs st}.|]

-- | Console, Worktree, Exec, RepoEvent, and Journal, exercised in one loop
-- with no model round: proves the driver services all five suspension kinds.
loop :: State -> Harness State
loop st = do
  say "outer-effects probe starting"
  wtResult <- createWorktree (fromCurrentRepository "outer-effects-probe")
  case wtResult of
    Left e -> pure st {runs = st.runs + 1, lastError = renderWorktreeError e}
    Right wt -> do
      Right proc <- run "echo outer-effects-probe"
      withHandler
        (headChanged wt)
        (\_change -> say "observed a head change")
        (pure ())
      record "outer-effects" "probe" (object ["exec" .= proc.stdout])
      pure
        st
          { runs = st.runs + 1
          , execOutput = proc.stdout
          }
