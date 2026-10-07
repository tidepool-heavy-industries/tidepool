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
  , result, settlement, eitherOf, after, afterSited, await
  , watch, pollWatch, forgetWatch
  , Route, RouteState (..), route, pollRoute, listRoutes, forgetRoute
  , requireObserved
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Tidepool.Internal.RequestSite (RequestSite)
import Prelude

import Tidepool.Effects.Core (CommandReport)
import Tidepool.Agent.Reply.Internal
  ( ReplyError (..), Progress (..), ProgressCursor (..), ProgressState (..)
  , PendingProgress (..), RequestId (..), Request, ResponseFailure
  , ResponseResult (responseValue), readResponse, responseRequestId
  )

data AwaitDependency
  = AwaitDependency RequestId Bool
  | AwaitProgress RequestId ProgressCursor
  | AwaitCommand Text
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

data Await a = Await AwaitPlan
  (forall effects. Member Watches effects => Int -> Int -> AwaitDecision -> Eff effects a)

instance Functor Await where
  fmap f (Await plan observe) = Await plan (\watchId offset decision -> f <$> observe watchId offset decision)

instance Applicative Await where
  pure value = Await (AwaitPlan [ReadyNode] 0) (\_ _ _ -> pure value)
  Await left observeFunction <*> Await right observeArgument =
    let (plan, rightOffset, _) = combine AllNode left right
    in Await plan $ \watchId offset decision ->
      observeFunction watchId offset decision <*> observeArgument watchId (offset + rightOffset) decision

-- | Select the first terminal branch, including failure. Already terminal
-- ties prefer the left. A nested choice remains fixed while its parent waits.
eitherOf :: Await a -> Await b -> Await (Either a b)
eitherOf (Await left observeLeft) (Await right observeRight) =
  let (plan, rightOffset, choiceNode) = combine EitherNode left right
  in Await plan $ \watchId offset decision@(AwaitDecision _ choices) ->
    case lookup (offset + choiceNode) choices of
      Just True -> Left <$> observeLeft watchId offset decision
      Just False -> Right <$> observeRight watchId (offset + rightOffset) decision
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
  RegisterRouteWith :: Text -> (Int -> Eff effects ()) -> AwaitPlan -> Watches Int
  ObserveRouteWith :: Int -> Watches RouteState
  ListRoutesWith :: Watches [Int]
  ObserveWatchProgressWith :: RequestSite '[progress] (ProgressState progress) -> Int -> Int -> Int -> Watches (ProgressState progress)
  ObserveWatchWith :: Int -> Watches RawWatchObservation
  AwaitWatchWith :: Int -> Watches RawWatchObservation
  ObserveWatchCommandWith :: Int -> Text -> Watches (Maybe CommandReport)
  ForgetWatchWith :: Int -> Watches ForgetWatchOutcome

data ForgetWatchOutcome = WatchForgotten | WatchForgetPending | WatchForgetRejected ReplyError
  deriving (Show, Eq)

-- | Project a successful value. Dependency failures are returned by 'await'.
result :: Request a -> Await a
result request = Await (AwaitPlan [LeafNode (AwaitDependency (responseRequestId request) False)] 0)
  (\_ _ _ -> pure (responseValue (requireObserved (readResponse request))))

-- | Capture settlement failure as an ordinary value, allowing all outcomes
-- to be collected with traverse through the same evaluator.
settlement :: Request a -> Await (Either ResponseFailure a)
settlement request = Await (AwaitPlan [LeafNode (AwaitDependency (responseRequestId request) True)] 0)
  (\_ node (AwaitDecision leaves _) -> pure $ case lookup node leaves of
    Just (Just failure) -> Left failure
    Just Nothing -> Right (responseValue (requireObserved (readResponse request)))
    Nothing -> error "await: terminal decision omitted its selected settlement")

requireObserved :: Maybe a -> a
requireObserved (Just value) = value
requireObserved Nothing = error "await: retained successful value is unavailable"

{-# OPAQUE after #-}
after :: forall progress. Progress progress -> ProgressCursor -> Await (ProgressState progress)
after = afterSited (error "after: extractor must assign a typed site")

{-# OPAQUE afterSited #-}
afterSited :: forall progress. RequestSite '[progress] (ProgressState progress) -> Progress progress -> ProgressCursor -> Await (ProgressState progress)
afterSited site (Progress request@(RequestId requestId)) cursor@(ProgressCursor revision) =
  Await (AwaitPlan [LeafNode (AwaitProgress request cursor)] 0) $ \watchId _ _ ->
    send (ObserveWatchProgressWith site watchId requestId revision)

watch :: Member Watches effects => Maybe Text -> Await a -> Eff effects (Watch a)
watch label awaiting@(Await plan _) = do
  identity <- send (RegisterWatchWith (maybe "" id label) plan)
  pure (Watch (WatchId identity) awaiting)

pollWatch :: Member Watches effects => Watch a -> Eff effects (WatchState a)
pollWatch (Watch (WatchId identity) (Await _ observe)) = do
  observation <- send (ObserveWatchWith identity)
  project identity observe observation

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
      projected <- project identity observe observation
      case projected of
        WatchPending _ -> loop
        WatchReady value -> pure (Right value)
        WatchUnavailable failure -> pure (Left failure)

project :: Member Watches effects => Int -> (Int -> Int -> AwaitDecision -> Eff effects a) -> RawWatchObservation -> Eff effects (WatchState a)
project identity observe observation = case observation of
  RawWatchPending progress -> pure (WatchPending progress)
  RawWatchReady decision -> WatchReady <$> observe identity 0 decision
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
