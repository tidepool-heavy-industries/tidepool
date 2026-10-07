-- | Compositional readiness over singular typed requests.
module Tidepool.Agent.Watch
  ( Await, AwaitError (..), result, settlement, eitherOf, after, await
  , Watch, WatchId, Watches, WatchState (..)
  , watch, pollWatch, forgetWatch, ForgetWatchOutcome (..)
  , Route, RouteState (..), route, pollRoute, listRoutes, forgetRoute
  ) where

import Tidepool.Agent.Watch.Internal
