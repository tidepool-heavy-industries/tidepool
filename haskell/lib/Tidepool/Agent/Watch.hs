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
  , settledValue
  , awaitResponse
  , awaitValue
  , awaitSettled
  , awaitProgressAfter
  , awaitAnyProgress
  , watch
  , Route
  , RouteState (..)
  , route
  , pollRoute
  , listRoutes
  , forgetRoute
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
  , settledValue
  , awaitResponse
  , awaitValue
  , awaitSettled
  , awaitProgressAfter
  , awaitAnyProgress
  , pollWatch
  , ForgetWatchOutcome (..)
  , forgetWatch
  , watch
  , Route
  , RouteState (..)
  , route
  , pollRoute
  , listRoutes
  , forgetRoute
  , watchLabel
  )
