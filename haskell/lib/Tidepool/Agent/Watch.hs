{-# LANGUAGE FlexibleContexts #-}

-- | Typed, nonblocking readiness subscriptions over agent responses.
module Tidepool.Agent.Watch
  ( Await
  , Watch
  , WatchId
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
  , awaitProgressAfter
  , watch
  , pollWatch
  , ForgetWatchOutcome (..)
  , forgetWatch
  ) where

import Tidepool.Agent.Watch.Internal
  ( Await
  , Watch
  , WatchId
  , WatchLabel
  , WatchLabelError (..)
  , Watches
  , WatchFailure (..)
  , WatchState (..)
  , Settlement (..)
  , awaitResponse
  , awaitValue
  , awaitSettled
  , awaitProgressAfter
  , pollWatch
  , ForgetWatchOutcome (..)
  , forgetWatch
  , watch
  , watchLabel
  )
