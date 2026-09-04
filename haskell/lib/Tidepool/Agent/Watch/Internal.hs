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
  | RawWatchReady
  | RawWatchUnavailable RequestId ResponseFailure
  | RawWatchRejected ReplyError

data Watches a where
  RegisterWatchWith :: Text -> [Int] -> Watches Int
  ObserveWatchWith :: Int -> Watches RawWatchObservation

awaitSettled :: Response result -> Await (ResponseResult result)
awaitSettled response =
  Await [responseRequestId response] (const (readResponse response))

awaitValue :: Response result -> Await result
awaitValue = fmap responseValue . awaitSettled

-- | Compatibility spelling for callers interested only in the authored value.
awaitResponse :: Response result -> Await result
awaitResponse = awaitValue

watch :: Member Watches effs => WatchLabel -> Await result -> Eff effs (Watch result)
watch (WatchLabel label) awaiting@(Await dependencies _) = do
  watchId <- send (RegisterWatchWith label (map unRequestId dependencies))
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
