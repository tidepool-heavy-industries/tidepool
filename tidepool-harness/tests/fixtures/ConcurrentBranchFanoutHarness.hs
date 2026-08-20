{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for the BULK sibling verb, `runLLMTurnBranchFanout`
-- (operator decision: sibling branch windows are ALWAYS driven concurrently,
-- transparently — scheduling is never a model-visible choice). ROOT freezes
-- its own window, then forks NINE labeled children off that ONE
-- `ContextRef` in a SINGLE bulk call — proving the same three contracts
-- `tidepool-harness/tests/outer_fanout.rs` pins for plain `runLLMTurnFanout`
-- carry over to the branch-shaped verb: children are serviced CONCURRENTLY
-- (completion order never reaches the observable result), the concurrency
-- cap is respected, and one child's abnormal exit folds as DATA at its own
-- branch position without erasing its siblings' answers (PRD 21 locked
-- decision 6).
--
-- Nine children (not two), for the same reason `ConcurrentFanoutHarness`
-- picks nine: ONE fixture/compile serves the order-insensitivity assertion
-- (which only inspects two of the nine), the concurrency-cap assertion
-- (which needs a fan wider than the default cap of 8), and the typed-exit
-- assertion (which starves ONE child of rounds and checks its siblings
-- still arrive).
--
-- `runLLMTurnBranchFanout @Int ref labeledPrompts` answers
-- @[Either InvocationExit (Int, ContextRef)]@ — one result per `(label,
-- prompt)` pair, in DECLARED order, each position independently either an
-- answer (plus a ref to that child's own post-finalize prefix) or the typed
-- reason that window ended without one. The fixture keeps both projections
-- in its state — `answers` (what arrived) and `outcomes` (one entry per
-- branch position) — the same shape `ContextRefHarness` uses for the
-- single-branch verb.
module ConcurrentBranchFanoutHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Effects
  ( ContextRef
  , InvocationExit
  , freezeContext
  , renderInvocationExit
  , runLLMTurnBranchFanout
  )
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

data State = State
  { loopCount :: Int
  , answers   :: [Int]
  , outcomes  :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {loopCount = 0, answers = [], outcomes = []}

render :: State -> Text
render st =
  [fmt|Concurrent branch-fanout harness. Loop count: {loopCount st}.|]

-- | Freeze ROOT's window immediately, then fork NINE labeled children off
-- the SAME frozen prefix in one bulk call, answered CONCURRENTLY by the
-- driver — each in its own freshly-minted answerer realm, up to the
-- driver's concurrency cap.
loop :: State -> Harness State
loop st = do
  ref <- freezeContext
  results <-
    runLLMTurnBranchFanout @Int
      ref
      [ ("root/1-child", "BRANCH-0")
      , ("root/2-child", "BRANCH-1")
      , ("root/3-child", "BRANCH-2")
      , ("root/4-child", "BRANCH-3")
      , ("root/5-child", "BRANCH-4")
      , ("root/6-child", "BRANCH-5")
      , ("root/7-child", "BRANCH-6")
      , ("root/8-child", "BRANCH-7")
      , ("root/9-child", "BRANCH-8")
      ]
  pure st { loopCount = loopCount st + 1
          , answers = answered results
          , outcomes = map renderOutcome results
          }

-- | The branch answers that arrived, in branch order — a window that exited
-- without one contributes nothing here and does NOT displace its siblings.
answered :: [Either InvocationExit (Int, ContextRef)] -> [Int]
answered = foldr keep []
  where
    keep (Right (n, _)) acc = n : acc
    keep (Left _)       acc = acc

-- | One line per BRANCH POSITION, so a failure is legible where it happened
-- rather than as a hole in the answer list.
renderOutcome :: Either InvocationExit (Int, ContextRef) -> Text
renderOutcome (Right (n, _)) = "ok:" <> show n
renderOutcome (Left e)       = "exit:" <> renderInvocationExit e
