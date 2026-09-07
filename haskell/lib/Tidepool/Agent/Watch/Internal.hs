{-# LANGUAGE DeriveFunctor #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE RankNTypes #-}
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
  , awaitProgressAfter
  , watch
  , Route
  , RouteState (..)
  , route
  , pollRoute
  , forgetRoute
  , pollWatch
  , ForgetWatchOutcome (..)
  , forgetWatch
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Char (isAsciiLower, isDigit)
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude

import Tidepool.Agent.Reply.Internal
  ( ReplyError
  , Progress (..)
  , ProgressCursor (..)
  , ProgressState
  , RequestId (..)
  , Response
  , ResponseFailure
  , ResponseResult (responseValue)
  , readResponse
  , responseRequestId
  )

data AwaitDependency = AwaitDependency RequestId Bool | AwaitProgress RequestId ProgressCursor
  deriving (Eq)

data Await result = Await [AwaitDependency]
  (forall effs. Member Watches effs => Int -> [(RequestId, ResponseFailure)] -> Eff effs (Maybe result))

instance Functor Await where
  fmap f (Await dependencies observe) =
    Await dependencies (\watchId failures -> fmap (fmap f) (observe watchId failures))

instance Applicative Await where
  pure value = Await [] (\_ _ -> pure (Just value))
  Await leftDependencies observeFunction <*> Await rightDependencies observeArgument =
    Await
      (deduplicate (leftDependencies <> rightDependencies))
      (\watchId failures -> do
        function <- observeFunction watchId failures
        argument <- observeArgument watchId failures
        pure (function <*> argument))

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
  deriving (Show, Eq, Functor)

data RawWatchObservation
  = RawWatchPending
  | RawWatchReady [(Int, ResponseFailure)]
  | RawWatchUnavailable RequestId ResponseFailure
  | RawWatchRejected ReplyError

data Watches a where
  RegisterWatchWith :: Text -> [AwaitDependency] -> Watches Int
  RegisterRouteWith :: Text -> (Int -> Eff effs ()) -> [AwaitDependency] -> Watches Int
  ObserveRouteWith :: Int -> Watches RouteState
  ObserveWatchProgressWith :: Int -> Int -> Int -> Watches (ProgressState progress)
  ObserveWatchWith :: Int -> Watches RawWatchObservation
  ForgetWatchWith :: Int -> Watches ForgetWatchOutcome

data ForgetWatchOutcome
  = WatchForgotten
  | WatchForgetPending
  | WatchForgetRejected ReplyError
  deriving (Show, Eq)

data Settlement result
  = ReplyAvailable (ResponseResult result)
  | ReplyUnavailable ResponseFailure
  deriving (Show, Eq)

awaitResponse :: Response result -> Await (ResponseResult result)
awaitResponse response =
  Await [AwaitDependency (responseRequestId response) False] (\_ _ -> pure (readResponse response))

awaitValue :: Response result -> Await result
awaitValue = fmap responseValue . awaitResponse

awaitSettled :: Response result -> Await (Settlement result)
awaitSettled response =
  Await [AwaitDependency request True] $ \_ failures ->
    pure $ case readResponse response of
      Just result -> Just (ReplyAvailable result)
      Nothing -> ReplyUnavailable <$> lookup request failures
  where
    request = responseRequestId response

awaitProgressAfter :: Progress progress -> ProgressCursor -> Await (ProgressState progress)
awaitProgressAfter (Progress request@(RequestId requestId)) cursor@(ProgressCursor revision) =
  Await [AwaitProgress request cursor] $ \watchId _ ->
    Just <$> send (ObserveWatchProgressWith watchId requestId revision)

watch :: Member Watches effs => WatchLabel -> Await result -> Eff effs (Watch result)
watch (WatchLabel label) awaiting@(Await dependencies _) = do
  watchId <- send (RegisterWatchWith label dependencies)
  pure (Watch (WatchId watchId) awaiting)

pollWatch
  :: Member Watches effs
  => Watch result
  -> Eff effs (WatchState result)
pollWatch (Watch (WatchId watchId) (Await _ observe)) = do
  observation <- send (ObserveWatchWith watchId)
  case observation of
    RawWatchPending -> pure WatchPending
    RawWatchReady rawFailures -> do
      captured <- observe watchId (map (\(request, failure) -> (RequestId request, failure)) rawFailures)
      pure $ case captured of
        Just result -> WatchReady result
        Nothing -> error "Tidepool watch became ready before every response cell was filled"
    RawWatchUnavailable request failure ->
      pure (WatchUnavailable (WatchDependencyUnavailable request failure))
    RawWatchRejected failure -> pure (WatchUnavailable (WatchRejected failure))

forgetWatch :: Member Watches effs => Watch result -> Eff effs ForgetWatchOutcome
forgetWatch (Watch (WatchId watchId) _) = send (ForgetWatchWith watchId)

deduplicate :: [AwaitDependency] -> [AwaitDependency]
deduplicate = foldr add []
  where
    add dependency rest
      | dependency `elem` rest = rest
      | otherwise = dependency : rest

-- | One watch-owned continuation, executed by the owning actor without inference.
newtype Route = Route Int deriving (Show, Eq)
data RouteState = RouteWaiting | RouteRunning | RouteCompleted | RouteFailed Text
  deriving (Show, Eq)

route
  :: Member Watches effs
  => Await (Settlement result)
  -> (Settlement result -> Eff effs ())
  -> Eff effs Route
route awaiting@(Await dependencies _) callback = do
  let entry watchId = do
        observed <- pollWatch (Watch (WatchId watchId) awaiting)
        case observed of
          WatchReady result -> callback result
          WatchUnavailable failure -> error (show failure)
          WatchPending -> error "route ran before settlement"
  Route <$> send (RegisterRouteWith "route" entry dependencies)

pollRoute :: Member Watches effs => Route -> Eff effs RouteState
pollRoute (Route watchId) = send (ObserveRouteWith watchId)

forgetRoute :: Member Watches effs => Route -> Eff effs ForgetWatchOutcome
forgetRoute (Route watchId) = send (ForgetWatchWith watchId)
