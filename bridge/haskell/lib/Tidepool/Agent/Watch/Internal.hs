{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveFunctor #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Typed readiness expressions and their retained runtime decisions.
module Tidepool.Agent.Watch.Internal
  ( Await (..), AwaitPlan (..), AwaitNode (..), AwaitDecision (..)
  , AwaitDependency (..), AwaitError (..)
  , Watch, WatchId (..), Watches (..), WatchState (..)
  , RawWatchObservation (..), ForgetWatchOutcome (..)
  , response, responseSited, settledResponse, settledResponseSited, result, resultSited, settlement, settlementSited, eitherOf, after, afterSited, observed, await
  , watch, pollWatch, forgetWatch
  , Route, RouteState (..), route, pollRoute, listRoutes, forgetRoute
  , requireObserved, Observation (..)
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Tidepool.Internal.RequestSite (RequestSite)
import Prelude

import Tidepool.Effects.Core (CommandReport)
import Tidepool.Agent.Reply.Internal
  ( ReplyError (..), Progress (..), ProgressCursor (..), ProgressState (..)
  , PendingProgress (..), RequestId (..), Request, ResponseFailure
  , ResponseResult (responseValue), responseRequestId
  )

data AwaitDependency
  = AwaitDependency RequestId Bool
  | AwaitProgress RequestId ProgressCursor
  | AwaitCommand Text
  | AwaitWatching Int
  deriving (Eq)

-- | Topological node references are local to this immutable expression.
data AwaitNode
  = ReadyNode
  | LeafNode AwaitDependency
  | AllNode Int Int
  | EitherNode Int Int
  deriving (Eq)

data AwaitPlan = AwaitPlan [AwaitNode] Int
  deriving (Eq)

-- | Only selected leaves and choices occur in a successful decision. A leaf's
-- failure is a value only when its settlement projection requested that.
data AwaitDecision = AwaitDecision [(Int, Maybe ResponseFailure)] [(Int, Bool)]

data Observation = Observation Int [Int]

data Await a = Await AwaitPlan
  (forall effects. Member Watches effects => Observation -> Int -> AwaitDecision -> Eff effects (Either AwaitError a))

instance Functor Await where
  fmap f (Await plan observe) = Await plan (\watchId offset decision -> fmap f <$> observe watchId offset decision)

instance Applicative Await where
  pure value = Await (AwaitPlan [ReadyNode] 0) (\_ _ _ -> pure (Right value))
  Await left observeFunction <*> Await right observeArgument =
    let (plan, rightOffset, _) = combine AllNode left right
    in Await plan $ \watchId offset decision ->
      do
        function <- observeFunction watchId offset decision
        case function of
          Left failure -> pure (Left failure)
          Right f -> fmap (fmap f) (observeArgument watchId (offset + rightOffset) decision)

-- | Select the first terminal branch, including failure. Already terminal
-- ties prefer the left. A nested choice remains fixed while its parent waits.
eitherOf :: Await a -> Await b -> Await (Either a b)
eitherOf (Await left observeLeft) (Await right observeRight) =
  let (plan, rightOffset, choiceNode) = combine EitherNode left right
  in Await plan $ \watchId offset decision@(AwaitDecision _ choices) ->
    case lookup (offset + choiceNode) choices of
      Just True -> fmap Left <$> observeLeft watchId offset decision
      Just False -> fmap Right <$> observeRight watchId (offset + rightOffset) decision
      Nothing -> error "await: terminal decision omitted its selected branch"

combine :: (Int -> Int -> AwaitNode) -> AwaitPlan -> AwaitPlan -> (AwaitPlan, Int, Int)
combine constructor (AwaitPlan left leftRoot) (AwaitPlan right rightRoot) =
  let rightOffset = length left
      nodes = left <> map (shiftNode rightOffset) right
      root = length nodes
  in (AwaitPlan (nodes <> [constructor leftRoot (rightOffset + rightRoot)]) root, rightOffset, root)

shiftNode :: Int -> AwaitNode -> AwaitNode
shiftNode offset (AllNode left right) = AllNode (offset + left) (offset + right)
shiftNode offset (EitherNode left right) = EitherNode (offset + left) (offset + right)
shiftNode _ node = node

newtype WatchId = WatchId Int deriving (Show, Eq, Ord)
data Watch a = Watch WatchId (Await a)
instance Show (Watch a) where
  show (Watch identity _) = "Watch " <> show identity

data AwaitError
  = AwaitDependencyUnavailable RequestId ResponseFailure
  | AwaitRejected ReplyError
  deriving (Show, Eq)

data WatchState a
  = WatchPending PendingProgress
  | WatchReady a
  | WatchUnavailable AwaitError
  deriving (Show, Eq, Functor)

data RawWatchObservation
  = RawWatchPending PendingProgress
  | RawWatchReady AwaitDecision
  | RawWatchUnavailable RequestId ResponseFailure
  | RawWatchRejected ReplyError

data Watches a where
  RegisterWatchWith :: Text -> AwaitPlan -> Watches Int
  RegisterAwaitWith :: AwaitPlan -> Watches (Either ReplyError Int)
  ReleaseAwaitWith :: Int -> Watches (Either ReplyError ())
  RegisterRouteWith :: Text -> (Int -> Eff effects ()) -> AwaitPlan -> Watches Int
  ObserveRouteWith :: Int -> Watches RouteState
  ListRoutesWith :: Watches [Int]
  ObserveWatchResponseWith :: RequestSite '[ResponseResult result] (Either ReplyError (ResponseResult result)) -> Int -> [Int] -> Int -> Watches (Either ReplyError (ResponseResult result))
  ObserveWatchProgressWith :: RequestSite '[progress] (ProgressState progress) -> Int -> [Int] -> Int -> Int -> Watches (ProgressState progress)
  ObserveWatchDecisionWith :: Int -> [Int] -> Watches AwaitDecision
  ObserveWatchWith :: Int -> Watches RawWatchObservation
  AwaitWatchWith :: Int -> Watches RawWatchObservation
  ObserveWatchCommandWith :: Int -> [Int] -> Text -> Watches (Maybe CommandReport)
  ForgetWatchWith :: Int -> Watches ForgetWatchOutcome

data ForgetWatchOutcome = WatchForgotten | WatchForgetPending | WatchForgetRejected ReplyError
  deriving (Show, Eq)

-- | Retain the original successful reply and its execution/worktree evidence.
-- Dependency failures are returned by 'await'.
{-# OPAQUE response #-}
response :: forall result. Request result -> Await (ResponseResult result)
response = responseSited (error "response: extractor must assign a typed site")

{-# OPAQUE responseSited #-}
responseSited :: forall result. RequestSite '[ResponseResult result] (Either ReplyError (ResponseResult result)) -> Request result -> Await (ResponseResult result)
responseSited site request = Await (AwaitPlan [LeafNode (AwaitDependency (responseRequestId request) False)] 0)
  (\(Observation identity path) node _ -> fmap (either (Left . AwaitRejected) Right)
    (send (ObserveWatchResponseWith site identity path node)))

-- | Capture terminal failure as a value while retaining successful receipts.
{-# OPAQUE settledResponse #-}
settledResponse :: forall result. Request result -> Await (Either ResponseFailure (ResponseResult result))
settledResponse = settledResponseSited (error "settledResponse: extractor must assign a typed site")

{-# OPAQUE settledResponseSited #-}
settledResponseSited :: forall result. RequestSite '[ResponseResult result] (Either ReplyError (ResponseResult result)) -> Request result -> Await (Either ResponseFailure (ResponseResult result))
settledResponseSited site request = Await (AwaitPlan [LeafNode (AwaitDependency (responseRequestId request) True)] 0)
  (\(Observation identity path) node (AwaitDecision leaves _) -> case lookup node leaves of
    Just (Just failure) -> pure (Right (Left failure))
    Just Nothing -> fmap (either (Left . AwaitRejected) (Right . Right))
      (send (ObserveWatchResponseWith site identity path node))
    Nothing -> error "await: terminal decision omitted its selected settlement")

-- | Project a successful value without changing its readiness or custody.
{-# OPAQUE result #-}
result :: forall result. Request result -> Await result
result = resultSited (error "result: extractor must assign a typed site")

{-# OPAQUE resultSited #-}
resultSited :: forall result. RequestSite '[ResponseResult result] (Either ReplyError (ResponseResult result)) -> Request result -> Await result
resultSited site = fmap responseValue . responseSited site

-- | Capture terminal failure as a value for ordinary traverse collection.
{-# OPAQUE settlement #-}
settlement :: forall result. Request result -> Await (Either ResponseFailure result)
settlement = settlementSited (error "settlement: extractor must assign a typed site")

{-# OPAQUE settlementSited #-}
settlementSited :: forall result. RequestSite '[ResponseResult result] (Either ReplyError (ResponseResult result)) -> Request result -> Await (Either ResponseFailure result)
settlementSited site = fmap (fmap responseValue) . settledResponseSited site

requireObserved :: Maybe a -> a
requireObserved (Just value) = value
requireObserved Nothing = error "await: retained successful value is unavailable"

{-# OPAQUE after #-}
after :: forall progress. Progress progress -> ProgressCursor -> Await (ProgressState progress)
after = afterSited (error "after: extractor must assign a typed site")

{-# OPAQUE afterSited #-}
afterSited :: forall progress. RequestSite '[progress] (ProgressState progress) -> Progress progress -> ProgressCursor -> Await (ProgressState progress)
afterSited site (Progress request@(RequestId requestId)) cursor@(ProgressCursor revision) =
  Await (AwaitPlan [LeafNode (AwaitProgress request cursor)] 0) $ \(Observation watchId path) _ _ ->
    Right <$> send (ObserveWatchProgressWith site watchId path requestId revision)

-- | Await the original watch's decision, retaining its selected values.
-- The source handle can be forgotten after this dependency is admitted.
observed :: Watch a -> Await a
observed (Watch (WatchId source) (Await _ observe)) =
  Await (AwaitPlan [LeafNode (AwaitWatching source)] 0) $ \(Observation identity path) node _ -> do
    let nestedPath = path <> [node]
    decision <- send (ObserveWatchDecisionWith identity nestedPath)
    observe (Observation identity nestedPath) 0 decision

watch :: Member Watches effects => Maybe Text -> Await a -> Eff effects (Watch a)
watch label awaiting@(Await plan _) = do
  identity <- send (RegisterWatchWith (maybe "" id label) plan)
  pure (Watch (WatchId identity) awaiting)

-- | Acquire a private subscription before observing. It owns projection
-- custody through the read, even if the public source watch is forgotten.
pollWatch :: Member Watches effects => Watch a -> Eff effects (WatchState a)
pollWatch source = do
  let Await plan observe = observed source
  registered <- send (RegisterAwaitWith plan)
  case registered of
    Left failure -> pure (WatchUnavailable (AwaitRejected failure))
    Right identity -> do
      observation <- send (ObserveWatchWith identity)
      projected <- project (Observation identity []) observe observation
      _ <- send (ReleaseAwaitWith identity)
      pure projected

-- | The sole readiness evaluator. Its transient subscription is runtime-owned
-- and cancellation releases that subscription without cancelling requests.
await :: Member Watches effects => Await a -> Eff effects (Either AwaitError a)
await awaiting@(Await plan _) = do
  registered <- send (RegisterAwaitWith plan)
  case registered of
    Left failure -> pure (Left (AwaitRejected failure))
    Right identity -> do
      let subscription = Watch (WatchId identity) awaiting
      outcome <- waitSubscription subscription
      _ <- forgetWatch subscription
      pure outcome

waitSubscription :: Member Watches effects => Watch a -> Eff effects (Either AwaitError a)
waitSubscription (Watch (WatchId identity) (Await _ observe)) = loop
  where
    loop = do
      observation <- send (AwaitWatchWith identity)
      projected <- project (Observation identity []) observe observation
      case projected of
        WatchPending _ -> loop
        WatchReady value -> pure (Right value)
        WatchUnavailable failure -> pure (Left failure)

project :: Member Watches effects => Observation -> (Observation -> Int -> AwaitDecision -> Eff effects (Either AwaitError a)) -> RawWatchObservation -> Eff effects (WatchState a)
project view observe observation = case observation of
  RawWatchPending progress -> pure (WatchPending progress)
  RawWatchReady decision -> either WatchUnavailable WatchReady <$> observe view 0 decision
  RawWatchUnavailable request failure -> pure (WatchUnavailable (AwaitDependencyUnavailable request failure))
  RawWatchRejected failure -> pure (WatchUnavailable (AwaitRejected failure))

forgetWatch :: Member Watches effects => Watch a -> Eff effects ForgetWatchOutcome
forgetWatch (Watch (WatchId identity) _) = send (ForgetWatchWith identity)

newtype Route = Route Int deriving (Show, Eq)
data RouteState = RouteWaiting | RouteRunning | RouteCompleted | RouteFailed Text | RouteRejected ReplyError
  deriving (Show, Eq)

route :: Member Watches effects => Await a -> (a -> Eff effects ()) -> Eff effects Route
route awaiting@(Await plan _) callback = do
  let entry identity = do
        observed <- pollWatch (Watch (WatchId identity) awaiting)
        case observed of
          WatchReady value -> callback value
          WatchUnavailable failure -> error (show failure)
          WatchPending _ -> error "route ran before its expression was terminal"
  Route <$> send (RegisterRouteWith "" entry plan)

pollRoute :: Member Watches effects => Route -> Eff effects RouteState
pollRoute (Route identity) = send (ObserveRouteWith identity)
forgetRoute :: Member Watches effects => Route -> Eff effects ForgetWatchOutcome
forgetRoute (Route identity) = send (ForgetWatchWith identity)
listRoutes :: Member Watches effects => Eff effects [Route]
listRoutes = map Route <$> send ListRoutesWith
