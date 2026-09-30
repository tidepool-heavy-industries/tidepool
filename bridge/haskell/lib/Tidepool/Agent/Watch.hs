{-# LANGUAGE FlexibleContexts #-}

-- | Typed readiness subscriptions over agent responses.
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
  , awaitAnySettled
  , watch
  , Route
  , RouteState (..)
  , route
  , pollRoute
  , listRoutes
  , forgetRoute
  , pollWatch
  , awaitWatch
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
  , awaitAnySettled
  , pollWatch
  , awaitWatch
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
