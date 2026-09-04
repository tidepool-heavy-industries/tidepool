{-# LANGUAGE FlexibleContexts #-}

-- | Typed, nonblocking readiness subscriptions over agent responses.
module Tidepool.Agent.Watch
  ( Await
  , Watch
  , WatchLabel
  , WatchLabelError (..)
  , watchLabel
  , Watches
  , WatchFailure (..)
  , WatchState (..)
  , Settlement (..)
  , awaitResponse
  , awaitValue
  , awaitSettled
  , watch
  , pollWatch
  ) where

import Tidepool.Agent.Watch.Internal
  ( Await
  , Watch
  , WatchLabel
  , WatchLabelError (..)
  , Watches
  , WatchFailure (..)
  , WatchState (..)
  , Settlement (..)
  , awaitResponse
  , awaitValue
  , awaitSettled
  , pollWatch
  , watch
  , watchLabel
  )
