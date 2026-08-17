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
-- Journal (@record@), and (PRD 20 S1-L4) 'Tidepool.Async' — DIRECTLY (no
-- model round at all — the loop is authored orchestration), so the driver's
-- suspension-servicing paths (including the green-thread scheduler,
-- 'SelfHarnessDriver.service_green_hole') are the only thing under test.
--
-- @Green@ being in 'outer_decls' makes 'Tidepool.Async' auto-import into the
-- OUTER TURN compile (`tidepool-mcp/src/effect_defs.rs`'s
-- @extra_imports_for!@) — but this harness source is spliced as a plain
-- @--include@d module (`HarnessSource`'s own module doc), not compiled as a
-- turn, so it needs the ordinary explicit import below like any other
-- symbol this module uses.
--
-- Every 'Tidepool.Async' payload here is 'Int', deliberately: it keeps this
-- bundle's assertions about the SCHEDULER (parking, waking, ordering) free
-- of an unrelated packed-`Text`-literal JIT construction path this lane does
-- not own.
module OuterEffectsHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Async
  ( AsyncCancelled (..)
  , async
  , cancel
  , mapConcurrently
  , wait
  , waitCatch
  , waitEither
  )
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
  , asyncOne :: Int
  , asyncRaceWinner :: Text
  , asyncLoserVal :: Int
  , asyncCancelled :: Bool
  , asyncMapResults :: [Int]
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState =
  State
    { runs = 0
    , lastError = ""
    , execOutput = ""
    , asyncOne = 0
    , asyncRaceWinner = ""
    , asyncLoserVal = 0
    , asyncCancelled = False
    , asyncMapResults = []
    }

render :: State -> Text
render st =
  [fmt|Outer-effects harness. Runs: {runs st}.|]

-- | One `mapConcurrently` element's work: a PURE recursive sum whose length
-- scales with `n` — deliberately DIFFERING lengths (3 vs 1 vs 2 reduction
-- steps for the `[3, 1, 2]` list below), so the three threads are not
-- interchangeable. `$!` forces the result before it settles — see
-- `Tidepool.Async`'s own module doc ("a non-closure result should be
-- forced"). Single-level only: nested `async` (a green thread's body
-- spawning another thread) is a known gap this lane's submit note flags,
-- not something this bundle exercises.
mapWork :: Int -> Harness Int
mapWork n = pure $! sumTo n * 10
  where
    sumTo 0 = 0
    sumTo k = k + sumTo (k - 1)

-- | Console, Worktree, Exec, RepoEvent, Journal, and 'Tidepool.Async',
-- exercised in one loop with no model round: proves the driver services
-- every suspension kind, including the green-thread scheduler.
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

      -- `wait`: fork one thread, join it, get its value back.
      hOne <- async (pure (21 :: Int))
      one <- wait hOne

      -- `waitEither`: race two threads (identified by distinct Int
      -- payloads: 1 = "A", 2 = "B"). `waitEither` never cancels (unlike
      -- `race`), so the LOSER must still be joinable afterward — proven by
      -- successfully `wait`ing it too, not by inspecting timing.
      hA <- async (pure (1 :: Int))
      hB <- async (pure (2 :: Int))
      raceResult <- waitEither hA hB
      loserVal <- case raceResult of
        Left _ -> wait hB
        Right _ -> wait hA

      -- `cancel` + `waitCatch`: a cancelled thread's `waitCatch` reports
      -- `Left AsyncCancelled` — its own pending suspensions (had it any past
      -- this point) are discarded via realm close, not merely marked.
      hC <- async (pure (99 :: Int))
      cancel hC
      cancelResult <- waitCatch hC

      -- `mapConcurrently`: threads of differing lengths (see `mapWork`),
      -- results come back in ORIGINAL list order regardless.
      mapResults <- mapConcurrently mapWork [3, 1, 2]

      pure
        st
          { runs = st.runs + 1
          , execOutput = proc.stdout
          , asyncOne = one
          , asyncRaceWinner = either (const "A") (const "B") raceResult
          , asyncLoserVal = loserVal
          , asyncCancelled = case cancelResult of
              Left AsyncCancelled -> True
              _ -> False
          , asyncMapResults = mapResults
          }
