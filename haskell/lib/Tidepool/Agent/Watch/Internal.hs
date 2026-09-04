{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Engine-private typed readiness subscriptions.
module Tidepool.Agent.Watch.Internal
  ( Await
  , Watch
  , WatchId (..)
  , WatchLabel (..)
  , WatchLabelError (..)
  , watchLabel
  , Watches (..)
  , WatchFailure (..)
  , WatchState (..)
  , Settlement (..)
  , RawWatchObservation (..)
  , awaitResponse
  , awaitValue
  , awaitSettled
  , watch
  , pollWatch
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Char (isAsciiLower, isDigit)
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude

import Tidepool.Agent.Reply.Internal
  ( ReplyError
  , RequestId (..)
  , Response
  , ResponseFailure
  , ResponseResult (responseValue)
  , readResponse
  , responseRequestId
  )

data AwaitDependency = AwaitDependency RequestId Bool

data Await result = Await [AwaitDependency] ([(RequestId, ResponseFailure)] -> Maybe result)

instance Functor Await where
  fmap f (Await dependencies observe) =
    Await dependencies (fmap f . observe)

instance Applicative Await where
  pure value = Await [] (const (Just value))
  Await leftDependencies observeFunction <*> Await rightDependencies observeArgument =
    Await
      (deduplicate (leftDependencies <> rightDependencies))
      (\failures -> observeFunction failures <*> observeArgument failures)

newtype WatchId = WatchId Int
  deriving (Show, Eq, Ord)

newtype WatchLabel = WatchLabel Text
  deriving (Show, Eq, Ord)

data WatchLabelError
  = EmptyWatchLabel
  | InvalidWatchLabel Text
  | WatchLabelTooLong Text
  deriving (Show, Eq)

watchLabel :: Text -> Either WatchLabelError WatchLabel
watchLabel value
  | Text.null value = Left EmptyWatchLabel
  | Text.length value > 48 = Left (WatchLabelTooLong value)
  | Text.head value == '-' || Text.last value == '-' = Left (InvalidWatchLabel value)
  | "--" `Text.isInfixOf` value = Left (InvalidWatchLabel value)
  | Text.all valid value = Right (WatchLabel value)
  | otherwise = Left (InvalidWatchLabel value)
  where
    valid character = isAsciiLower character || isDigit character || character == '-'

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
  | RawWatchReady [(Int, ResponseFailure)]
  | RawWatchUnavailable RequestId ResponseFailure
  | RawWatchRejected ReplyError

data Watches a where
  RegisterWatchWith :: Text -> [(Int, Bool)] -> Watches Int
  ObserveWatchWith :: Int -> Watches RawWatchObservation

data Settlement result
  = ReplyAvailable (ResponseResult result)
  | ReplyUnavailable ResponseFailure
  deriving (Show, Eq)

awaitResponse :: Response result -> Await (ResponseResult result)
awaitResponse response =
  Await [AwaitDependency (responseRequestId response) False] (const (readResponse response))

awaitValue :: Response result -> Await result
awaitValue = fmap responseValue . awaitResponse

awaitSettled :: Response result -> Await (Settlement result)
awaitSettled response =
  Await [AwaitDependency request True] $ \failures ->
    case readResponse response of
      Just result -> Just (ReplyAvailable result)
      Nothing -> ReplyUnavailable <$> lookup request failures
  where
    request = responseRequestId response

watch :: Member Watches effs => WatchLabel -> Await result -> Eff effs (Watch result)
watch (WatchLabel label) awaiting@(Await dependencies _) = do
  watchId <- send (RegisterWatchWith label (map rawDependency dependencies))
  pure (Watch (WatchId watchId) awaiting)

pollWatch
  :: Member Watches effs
  => Watch result
  -> Eff effs (WatchState result)
pollWatch (Watch (WatchId watchId) (Await _ observe)) = do
  observation <- send (ObserveWatchWith watchId)
  pure $ case observation of
    RawWatchPending -> WatchPending
    RawWatchReady rawFailures ->
      case observe (map (\(request, failure) -> (RequestId request, failure)) rawFailures) of
        Just result -> WatchReady result
        Nothing -> error "Tidepool watch became ready before every response cell was filled"
    RawWatchUnavailable request failure ->
      WatchUnavailable (WatchDependencyUnavailable request failure)
    RawWatchRejected failure -> WatchUnavailable (WatchRejected failure)

rawDependency :: AwaitDependency -> (Int, Bool)
rawDependency (AwaitDependency (RequestId request) allowFailure) = (request, allowFailure)

deduplicate :: [AwaitDependency] -> [AwaitDependency]
deduplicate = foldr add []
  where
    add dependency [] = [dependency]
    add (AwaitDependency request allowFailure) (AwaitDependency other otherAllows : rest)
      | request == other = AwaitDependency request (allowFailure && otherAllows) : rest
      | otherwise = AwaitDependency other otherAllows : add (AwaitDependency request allowFailure) rest
