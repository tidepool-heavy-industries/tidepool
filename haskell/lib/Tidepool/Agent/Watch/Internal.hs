{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}

-- | Engine-private typed readiness subscriptions.
module Tidepool.Agent.Watch.Internal
  ( Await
  , Watch
  , WatchId (..)
  , Watches (..)
  , WatchFailure (..)
  , WatchState (..)
  , RawWatchObservation (..)
  , awaitResponse
  , watch
  , pollWatch
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Prelude

import Tidepool.Agent.Reply.Internal
  ( ReplyError
  , RequestId (..)
  , Response
  , ResponseFailure
  , readResponse
  , responseRequestId
  )

data Await result = Await [RequestId] (() -> Maybe result)

instance Functor Await where
  fmap f (Await dependencies observe) =
    Await dependencies (fmap f . observe)

instance Applicative Await where
  pure value = Await [] (const (Just value))
  Await leftDependencies observeFunction <*> Await rightDependencies observeArgument =
    Await
      (deduplicate (leftDependencies <> rightDependencies))
      (\() -> observeFunction () <*> observeArgument ())

newtype WatchId = WatchId Int
  deriving (Show, Eq, Ord)

data Watch result = Watch WatchId (Await result)

instance Show (Watch result) where
  show (Watch watchId _) = "Watch " <> show watchId

data WatchFailure
  = WatchDependencyUnavailable RequestId ResponseFailure
  | WatchRejected ReplyError
  deriving (Show, Eq)

data WatchState result
  = WatchPending
  | WatchReady result
  | WatchUnavailable WatchFailure
  deriving (Show, Eq)

data RawWatchObservation
  = RawWatchPending
  | RawWatchReady
  | RawWatchUnavailable RequestId ResponseFailure
  | RawWatchRejected ReplyError

data Watches a where
  RegisterWatchWith :: [Int] -> Watches Int
  ObserveWatchWith :: Int -> Watches RawWatchObservation

awaitResponse :: Response result -> Await result
awaitResponse response =
  Await [responseRequestId response] (const (readResponse response))

watch :: Member Watches effs => Await result -> Eff effs (Watch result)
watch awaiting@(Await dependencies _) = do
  watchId <- send (RegisterWatchWith (map unRequestId dependencies))
  pure (Watch (WatchId watchId) awaiting)

pollWatch
  :: Member Watches effs
  => Watch result
  -> Eff effs (WatchState result)
pollWatch (Watch (WatchId watchId) (Await _ observe)) = do
  observation <- send (ObserveWatchWith watchId)
  pure $ case observation of
    RawWatchPending -> WatchPending
    RawWatchReady ->
      case observe () of
        Just result -> WatchReady result
        Nothing -> error "Tidepool watch became ready before every response cell was filled"
    RawWatchUnavailable request failure ->
      WatchUnavailable (WatchDependencyUnavailable request failure)
    RawWatchRejected failure -> WatchUnavailable (WatchRejected failure)

unRequestId :: RequestId -> Int
unRequestId (RequestId request) = request

deduplicate :: [RequestId] -> [RequestId]
deduplicate = foldr add []
  where
    add request requests
      | request `elem` requests = requests
      | otherwise = request : requests
