{-# LANGUAGE FlexibleContexts #-}

-- | Typed, nonblocking readiness subscriptions over agent responses.
module Tidepool.Agent.Watch
  ( Await
  , Watch
  , Watches
  , WatchFailure (..)
  , WatchState (..)
  , awaitResponse
  , awaitValue
  , awaitSettled
  , watch
  , pollWatch
  ) where

import Tidepool.Agent.Watch.Internal
  ( Await
  , Watch
  , Watches
  , WatchFailure (..)
  , WatchState (..)
  , awaitResponse
  , awaitValue
  , awaitSettled
  , pollWatch
  , watch
  )
