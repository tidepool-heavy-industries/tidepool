{-# LANGUAGE GADTs #-}

-- | Private representation shared by "Tidepool.Async" and its lightweight
-- public types module.
module Tidepool.Async.Internal
  ( Async (..)
  , asyncThreadId
  ) where

import Prelude

import Tidepool.Internal.ExitCell (ExitCell)

-- | A thread id paired with the managed cell holding its eventual result.
-- The pending type is existential: it is an implementation detail used only
-- to make each cell's allocation belong to its exact body computation.
data Async value where
  Async :: Int -> ExitCell pending value -> Async value

asyncThreadId :: Async value -> Int
asyncThreadId (Async threadId _) = threadId
