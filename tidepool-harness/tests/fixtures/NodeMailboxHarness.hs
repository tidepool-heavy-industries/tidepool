{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | The @Tidepool.Node@ capability-mailbox fixture (PRD 20 S1-L4 wave 2).
--
-- Standalone rather than folded into 'OuterEffectsHarness', for the same
-- reason 'NestedAsyncHarness' is: these scenarios currently hit the
-- tenure-then-resume GC family and are CRASH-CLASS, and the root @CLAUDE.md@
-- discipline keeps crash-class fixtures out of family bundles because a
-- bundled crash destroys its siblings\' diagnosis. Bundled, these would take
-- S1-L1\'s outer-row assertions and wave 1\'s whole green-thread acceptance
-- down with them.
--
-- ONE LEVEL ONLY: no node body here forks a node of its own. The GC failure
-- these hit does NOT require nesting — see @tests/nested_async_repro.rs@ for
-- the mechanism and the pass\/fail separator (closure-graph depth and
-- allocation volume, not nesting).
module NodeMailboxHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Async (wait)
import Tidepool.Effects (say)
-- `Observed`/`after`/`nextEvent`/`(<|>)` are DEFINITIONS in `Tidepool.Event`
-- (PRD 22 lane 4), not the generated `Tidepool.Effects` module — this fixture
-- is spliced rather than compiled as an ordinary turn, so it needs the
-- explicit import below like any other symbol this module uses.
import Tidepool.Event (Observed (..), after, nextEvent, (<|>))
import Tidepool.Node (folded, forkNode, received, sendUp, uplink)
-- `(<|>)`/`folded` hidden: `Tidepool.Prelude` re-exports base\'s `Alternative`
-- operator and `Control.Lens.Fold`\'s `folded` too, and this fixture wants the
-- Event-algebra names at every use site.
import Tidepool.Prelude hiding (render, folded, (<|>))
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

data State = State
  { runs :: Int
  , nodeMessage :: Int
  , nodeSilentTick :: Bool
  , nodeBurstPayload :: Int
  }
  deriving (Generic, Show, Eq, FromJSON, ToJSON)

initialState :: State
initialState =
  State
    { runs = 0
    , nodeMessage = 0
    , nodeSilentTick = False
    , nodeBurstPayload = 0
    }

render :: State -> Text
render st = [fmt|Node-mailbox probe. Runs: {runs st}.|]

loop :: State -> Harness State
loop st = do
  say "node-mailbox probe starting"
  -- One level only: no node body here forks a node of its own.
  --
  -- (1) A child `sendUp`s once; the parent's select over {message,
  -- deadline} observes the message before the (generous) deadline.
  nodeMsg <- forkNode @Int @Int (\ctx -> do
    sendUp (uplink ctx) (777 :: Int)
    pure (0 :: Int))
  msgDeadline <- after 5000
  Observed _ msgOutcome <-
    nextEvent (fmap Left (received nodeMsg) <|> fmap Right msgDeadline)
  nodeMsgVal <- case msgOutcome of
    Left v -> pure v
    Right _ -> pure (-1)

  -- (2) A SILENT child sends nothing; a short deadline elapses and the
  -- parent's select observes the Tick instead.
  nodeSilent <- forkNode @Int @Int (\_ctx -> pure (0 :: Int))
  silentDeadline <- after 30
  Observed _ silentOutcome <-
    nextEvent (fmap Left (received nodeSilent) <|> fmap Right silentDeadline)
  silentTick <- case silentOutcome of
    Left _ -> pure False
    Right _ -> pure True

  -- (3) A burst of same-tag (bare `Int`, one shared coalesce key) sends
  -- is observed ONCE, carrying the LAST payload. `folded`+`wait` first
  -- so the whole burst has already landed (and coalesced) before the
  -- select runs — deterministic, not a race against however many of the
  -- three sends the scheduler has serviced by the time the parent polls.
  nodeBurst <- forkNode @Int @Int (\ctx -> do
    sendUp (uplink ctx) (1 :: Int)
    sendUp (uplink ctx) (2 :: Int)
    sendUp (uplink ctx) (3 :: Int)
    pure (0 :: Int))
  Observed _ burstThread <- nextEvent (folded nodeBurst)
  _ <- wait burstThread
  burstDeadline <- after 5000
  Observed _ burstOutcome <-
    nextEvent (fmap Left (received nodeBurst) <|> fmap Right burstDeadline)
  burstVal <- case burstOutcome of
    Left v -> pure v
    Right _ -> pure (-1)

  pure
    st
      { runs = st.runs + 1
      , nodeMessage = nodeMsgVal
      , nodeSilentTick = silentTick
      , nodeBurstPayload = burstVal
      }
