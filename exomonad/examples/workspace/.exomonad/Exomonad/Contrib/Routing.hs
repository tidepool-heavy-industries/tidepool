{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE FlexibleContexts #-}

-- Typed evidence collection for live batches. Runtime owners retain ordered
-- delivery and lifetime; callers choose notification and observation policy.
module Exomonad.Contrib.Routing
  ( WorkActor (workSnapshot, workNotification, acknowledgeWork), WorkState (..), WorkSource (..), WorkStatus (..)
  , WorkEvent (..), WorkDelta (..), workChange, Notice (..), WorkSink (..), WorkDelivery (..), ObserverAdmission (..), noWorkDelivery, observeWork
  , WorkNoticePolicy (..), WorkPolicyReceipt (..), setWorkNoticePolicy
  , followWork, workDefinition, readWork, finishWork, keepWork, outstandingEvidence, outstandingReviewed
  , notifyWork, workNoticeMessage, workMessage, workQuestionsMessage, withCheckpoints
  , ReviewReadiness (..), reviewReadiness, reviewReadyMessage, notifyReviewReady
  , WorkBatchPlan, BatchFailure (..), RoutedBatch (..), WorkBatch (..)
  , projectWorkChildWith, projectWorkChild, workChild, unfoldWorkBatch, unfoldWork, unfoldWorkWith, finishWorkBatch, finishRoutedBatch
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.List (nub, (\\))
import GHC.Generics (Generic)
import qualified Tidepool.Actor.Record as R
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Tidepool.Actor as Actor
import Tidepool.Actors.Exomonad
import Tidepool.Worktree (renderGitOid)
import Tidepool.Effects.Core (Actor)
import Exomonad.Contrib.Types
import Exomonad.Contrib.Actors (CoordinationEffects, coordinationActor)

-- Applicative admission retains the original handle product. Only the event
-- projection changes a result's value; execution and worktree receipts survive.
data WorkBatchPlan effects event handles = WorkBatchPlan [Text]
  (Unfold effects (handles, WorkSources event))

data WorkSources value = WorkSources
  { progressEvents :: R.EventSource (Text, ProgressState WorkProgress)
  , resultEvents :: R.EventSource (Text, Either ResponseFailure (ResponseResult value))
  }

instance Semigroup (WorkSources value) where
  WorkSources lp lr <> WorkSources rp rr = WorkSources (lp <> rp) (lr <> rr)

instance Monoid (WorkSources value) where
  mempty = WorkSources mempty mempty

instance Functor (WorkBatchPlan effects event) where
  fmap f (WorkBatchPlan names admission) = WorkBatchPlan names
    (fmap (\(handles, sources) -> (f handles, sources)) admission)

instance Applicative (WorkBatchPlan effects event) where
  pure handles = WorkBatchPlan [] (pure (handles, mempty))
  WorkBatchPlan ln left <*> WorkBatchPlan rn right = WorkBatchPlan (ln ++ rn)
    ((\(f, ls) (x, rs) -> (f x, ls <> rs)) <$> left <*> right)

data BatchFailure = EmptyWorkBatch | EmptyWorkName | DuplicateWorkName Text
  | WorkAdmissionRefused UnfoldError
  deriving (Show, Eq)

validateBatch :: [Text] -> Either BatchFailure ()
validateBatch [] = Left EmptyWorkBatch
validateBatch names
  | any (Text.null . Text.strip) names = Left EmptyWorkName
  | otherwise = go [] names
  where
    go _ [] = Right ()
    go seen (name : rest)
      | name `elem` seen = Left (DuplicateWorkName name)
      | otherwise = go (name : seen) rest

data RoutedBatch handles event = RoutedBatch
  { routedMembers :: handles
  , routedCollector :: ActorHandle (WorkActor event)
  }

data WorkBatch value = WorkBatch
  { batchMembers :: [(Text, Response value, Progress WorkProgress)]
  , batchRouter :: ActorHandle (WorkActor value)
  }

{-# INLINE projectWorkChildWith #-}
projectWorkChildWith
  :: forall progress value event child input parent.
     (KnownEffects child, Subset child parent)
  => Text -> (progress -> WorkProgress) -> (value -> event)
  -> Branch child input value
  -> WorkBatchPlan parent event (Text, Response value, Progress progress)
projectWorkChildWith name projectProgress projectResult branch = WorkBatchPlan [name] $
  (\(response, progress) ->
    ((name, response, progress), WorkSources
      (fmap ((,) name . mapProgress projectProgress) (R.progress progress))
      (fmap ((,) name . fmap (mapResult projectResult)) (R.settlement response))))
    <$> childWithProgress @progress @value (withReport Silent branch)
  where
    mapProgress f observation = case observation of
      ProgressPending -> ProgressPending
      ProgressUpdate cursor value -> ProgressUpdate cursor (f value)
      ProgressClosed -> ProgressClosed
      ProgressRejected failure -> ProgressRejected failure
    mapResult f receipt = ResponseResult (f (responseValue receipt))
      (responseExecution receipt) (responseWorktree receipt)

{-# INLINE projectWorkChild #-}
projectWorkChild
  :: (KnownEffects child, Subset child parent)
  => Text -> (value -> event) -> Branch child input value
  -> WorkBatchPlan parent event (Text, Response value, Progress WorkProgress)
projectWorkChild name = projectWorkChildWith name id

{-# INLINE workChild #-}
workChild
  :: (KnownEffects child, Subset child parent)
  => Text -> Branch child input value
  -> WorkBatchPlan parent value (Text, Response value, Progress WorkProgress)
workChild name = projectWorkChild name id

unfoldWorkBatch
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects,
      Member Actor effects)
  => ForkGroupPath -> WorkBatchPlan effects event handles -> WorkSink event
  -> Eff effects (Either BatchFailure (RoutedBatch handles event))
unfoldWorkBatch group plan sink = fmap (fmap fst)
  (admitWorkBatch group plan (\_ -> pure (sink, ())))

admitWorkBatch
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects,
      Member Actor effects)
  => ForkGroupPath -> WorkBatchPlan effects event handles
  -> (handles -> Eff effects (WorkSink event, extra))
  -> Eff effects (Either BatchFailure (RoutedBatch handles event, extra))
admitWorkBatch group (WorkBatchPlan names admission) configure = case validateBatch names of
  Left issue -> pure (Left issue)
  Right () -> do
    admitted <- attemptUnfold group admission
    case admitted of
      Left issue -> pure (Left (WorkAdmissionRefused issue))
      Right (members, sources) -> do
        (sink, extra) <- configure members
        router <- R.start (sourceDefinition names sources sink)
        pure (Right (RoutedBatch members router, extra))

unfoldWork
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects,
      Member Actor effects)
  => ForkGroupPath
  -> [WorkBatchPlan effects value (Text, Response value, Progress WorkProgress)]
  -> WorkSink value -> Eff effects (Either BatchFailure (WorkBatch value))
unfoldWork group branches sink = fmap (fmap fst)
  (unfoldWorkWith group branches (\_ -> pure (sink, ())))

unfoldWorkWith
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects,
      Member Actor effects)
  => ForkGroupPath
  -> [WorkBatchPlan effects value (Text, Response value, Progress WorkProgress)]
  -> ([(Text, Response value, Progress WorkProgress)] -> Eff effects (WorkSink value, extra))
  -> Eff effects (Either BatchFailure (WorkBatch value, extra))
unfoldWorkWith group branches configure = fmap
  (fmap (\(RoutedBatch members router, extra) -> (WorkBatch members router, extra)))
  (admitWorkBatch group (sequenceA branches) configure)

-- Closing the collector is distinct from retiring children. Refuse while any
-- original request lacks a terminal observation, leaving its route intact.
finishWorkBatch
  :: Member Actor effects => WorkBatch value
  -> Eff effects (Either [Text] (Actor.ActorExit (WorkState value)))
finishWorkBatch = finishCollector . batchRouter

finishRoutedBatch
  :: Member Actor effects => RoutedBatch handles value
  -> Eff effects (Either [Text] (Actor.ActorExit (WorkState value)))
finishRoutedBatch = finishCollector . routedCollector

finishCollector
  :: Member Actor effects => ActorHandle (WorkActor value)
  -> Eff effects (Either [Text] (Actor.ActorExit (WorkState value)))
finishCollector router = do
  state <- readWork router
  case [sourceName source | source <- collectedWork state,
        Nothing <- [sourceResult source]] of
    [] -> Right <$> finishWork router
    pending -> pure (Left pending)

-- Closure keeps unanswered questions. The terminal response is independent of
-- progress closure and retains its execution/worktree evidence, including failure.
data WorkStatus = WorkOpen | WorkClosed | WorkRejected ReplyError
  deriving (Show, Eq)

data WorkSource value = WorkSource
  { sourceName :: Text
  , sourceProgress :: WorkProgress
  , sourceCursor :: Maybe ProgressCursor
  , sourceStatus :: WorkStatus
  , sourceResult :: Maybe (Either ResponseFailure (ResponseResult value))
  } deriving (Show)

data WorkDelta = WorkDelta
  { deltaCursor :: ProgressCursor
  , addedEvidence :: [Candidate]
  , addedReviewed :: [ReviewedCheckpoint]
  , openedQuestions :: Attention
  , resolvedQuestions :: Attention
  } deriving (Show)

workChange :: Text -> ProgressCursor -> WorkProgress -> WorkProgress -> WorkEvent value
workChange name cursor previous current = WorkChanged name WorkDelta
  { deltaCursor = cursor
  , addedEvidence = workEvidence current \\ workEvidence previous
  , addedReviewed = workReviewed current \\ workReviewed previous
  , openedQuestions = workQuestions current \\ workQuestions previous
  , resolvedQuestions = [q | q <- workQuestions previous,
      not (any (sameQuestion q) (workQuestions current))]
  }

data WorkEvent value
  = WorkChanged Text WorkDelta
  | WorkEnded Text WorkStatus
  | WorkFinished Text (Either ResponseFailure (ResponseResult value))
  | WorkPolicyChanged WorkNoticePolicy WorkNoticePolicy
  deriving (Show)

data WorkNoticePolicy = QuestionsAndResults | IncludeReviewed | AllCheckpoints
  deriving (Show, Eq)

data WorkPolicyReceipt = WorkPolicyReceipt
  { policyEvent :: Maybe Int
  , previousPolicy :: WorkNoticePolicy
  , currentPolicy :: WorkNoticePolicy
  } deriving (Show, Eq)

-- Successful admission receipts and failures are both retained. Nothing here
-- automatically retries an uncertain send or claims model incorporation.
data Notice = Notice
  { noticeEvent :: Int
  , noticeReceipt :: Either NotificationError NotificationReceipt
  }

instance Show Notice where
  show notice = "Notice " ++ show (noticeEvent notice) ++ " "
    ++ either show (const "NotificationReceipt") (noticeReceipt notice)

data WorkState value = WorkState
  { collectedWork :: [WorkSource value]
  , workNotices :: [Notice]
  , workHistory :: [WorkEvent value]
  , handledWork :: [(Text, Candidate)]
  , workNoticePolicy :: WorkNoticePolicy
  , workObserverAdmissions :: [ObserverAdmission]
  }

-- Binding a snapshot should not print whole tasks or accumulated receipts.
-- Inspect collectedWork/workNotices explicitly when their evidence is needed.
instance Show (WorkState value) where
  show state = "WorkState " ++ show
    [ (sourceName source, length (workEvidence (sourceProgress source)),
       length (workQuestions (sourceProgress source)), sourceStatus source,
       case sourceResult source of { Nothing -> False; Just _ -> True })
    | source <- collectedWork state
    ] ++ " notices=" ++ show (length (workNotices state))
      ++ " policy=" ++ show (workNoticePolicy state)

data WorkActor value mode = WorkActor
  { workState :: mode :- State (WorkState value)
  , workSnapshot :: mode :- Call () (R.Reply (WorkState value))
  , workNotification :: mode :- Call NotificationReceipt (R.Reply (Either NotificationError NotificationState))
  , acknowledgeWork :: mode :- Call (Text, [Candidate]) NoReply
  , workPolicy :: mode :- Call WorkNoticePolicy (R.Reply WorkPolicyReceipt)
  , workUpdates :: mode :- Event (Text, ProgressState WorkProgress)
  , workResults :: mode :- Event (Text, Either ResponseFailure (ResponseResult value))
  } deriving Generic

type WorkEffects value = CoordinationEffects (WorkActor value)
-- A primary authored policy has ordinary actor failure semantics. Optional
-- observer admission is separate and cannot throw on a refused mailbox.
data WorkDelivery = WorkDelivery
  { primaryWorkNotice :: Maybe (Either NotificationError NotificationReceipt)
  , observerWorkAdmissions :: [(Text, Either Text ())]
  }

data ObserverAdmission = ObserverAdmission
  { observerEvent :: Int
  , observerName :: Text
  , observerAdmission :: Either Text ()
  } deriving (Show, Eq)

noWorkDelivery :: WorkDelivery
noWorkDelivery = WorkDelivery Nothing []

newtype WorkSink value = WorkSink
  { runWorkSink :: forall effects.
      (Member Actor effects, Member Replies effects, Member Notifications effects)
      => WorkNoticePolicy -> WorkEvent value -> Eff effects WorkDelivery
  }

keepWork :: WorkSink value
keepWork = WorkSink (\_ _ -> pure noWorkDelivery)

observeWork
  :: Text -> R.Send event -> (WorkEvent value -> Maybe event)
  -> WorkSink value -> WorkSink value
observeWork name endpoint project (WorkSink sink) = WorkSink $ \policy event -> do
  delivered <- sink policy event
  case project event of
    Nothing -> pure delivered
    Just projected -> do
      admitted <- R.trySend endpoint projected
      pure delivered { observerWorkAdmissions = observerWorkAdmissions delivered ++ [(name, admitted)] }

followWork
  :: Member Actor effects
  => [(Text, Response value, Progress WorkProgress)] -> WorkSink value
  -> Eff effects (Either BatchFailure (ActorHandle (WorkActor value)))
followWork inputs sink = case workDefinition inputs sink of
  Left issue -> pure (Left issue)
  Right spec -> Right <$> R.start spec

readWork :: Member Actor effects => ActorHandle (WorkActor value) -> Eff effects (WorkState value)
readWork router = R.call (workSnapshot (R.client router)) ()

-- A policy change is ordered with source events in this actor. It changes
-- future sends only; callers can inspect 'outstandingReviewed' explicitly.
setWorkNoticePolicy
  :: Member Actor effects
  => ActorHandle (WorkActor value) -> WorkNoticePolicy -> Eff effects WorkPolicyReceipt
setWorkNoticePolicy router policy = R.call (workPolicy (R.client router)) policy

finishWork :: Member Actor effects => ActorHandle (WorkActor value) -> Eff effects (Actor.ActorExit (WorkState value))
finishWork = R.finish

workDefinition
  :: forall value. [(Text, Response value, Progress WorkProgress)] -> WorkSink value
  -> Either BatchFailure (ActorSpec (WorkActor value) (WorkEffects value))
workDefinition inputs sink = do
  let names = [name | (name, _, _) <- inputs]
  validateBatch names
  pure (sourceDefinition names (WorkSources
    (mconcat [fmap ((,) name) (R.progress updates) | (name, _, updates) <- inputs])
    (mconcat [fmap ((,) name) (R.settlement response) | (name, response, _) <- inputs])) sink)

sourceDefinition
  :: forall value. [Text] -> WorkSources value -> WorkSink value
  -> ActorSpec (WorkActor value) (WorkEffects value)
sourceDefinition names sources (WorkSink sink) = coordinationActor "work" WorkActor
      { workState = WorkState [WorkSource name (WorkProgress [] []) Nothing WorkOpen Nothing | name <- names] [] [] [] QuestionsAndResults []
      , workSnapshot = \() -> R.get
      , workNotification = pollNotification
      , acknowledgeWork = \(name, candidates) -> R.modify' (\state -> state
          { handledWork = nub (handledWork state ++ [(name, candidate) | candidate <- candidates]) })
      , workPolicy = \policy -> do
          state <- R.get
          let before = workNoticePolicy state
          if before == policy then pure (WorkPolicyReceipt Nothing before policy) else do
            let index = length (workHistory state)
            R.put (state { workNoticePolicy = policy
              , workHistory = workHistory state ++ [WorkPolicyChanged before policy] })
            pure (WorkPolicyReceipt (Just index) before policy)
      , workUpdates = R.on (progressEvents sources) update
      , workResults = R.on (resultEvents sources) settled
      }
  where
    update (name, observation) = do
      state <- R.get
      case observation of
        ProgressPending -> R.put (ensure name state)
        ProgressUpdate cursor progress -> do
          let current = findSource name state
              previous = sourceProgress current
              next = mergeWorkProgress previous progress
          R.put (putSource (current { sourceProgress = next, sourceCursor = Just cursor }) state)
          publish (workChange name cursor previous next)
        ProgressClosed -> ended name WorkClosed
        ProgressRejected failure -> ended name (WorkRejected failure)
    settled (name, result) = do
      R.modify' (\state -> putSource ((findSource name state) { sourceResult = Just result }) state)
      publish (WorkFinished name result)
    ended name status = do
      state <- R.get
      let current = findSource name state
      R.put (putSource (current { sourceStatus = status }) state)
      if sourceStatus current == status then pure () else publish (WorkEnded name status)
    publish event = do
      state <- R.get
      let index = length (workHistory state)
      R.put (state { workHistory = workHistory state ++ [event] })
      sent <- sink (workNoticePolicy state) event
      R.modify' (\current -> current { workNotices = case primaryWorkNotice sent of
        Nothing -> workNotices current
        Just receipt -> workNotices current ++ [Notice index receipt]
        , workObserverAdmissions = workObserverAdmissions current ++
            [ObserverAdmission index name receipt | (name, receipt) <- observerWorkAdmissions sent] })

outstandingEvidence :: WorkState value -> WorkSource value -> [Candidate]
outstandingEvidence state source =
  [candidate | candidate <- workEvidence (sourceProgress source),
    (sourceName source, candidate) `notElem` handledWork state]

outstandingReviewed :: WorkState value -> [(Text, ReviewedCheckpoint)]
outstandingReviewed state =
  [ (sourceName source, checkpoint)
  | source <- collectedWork state
  , checkpoint <- workReviewed (sourceProgress source)
  , (sourceName source, checkpointCandidate checkpoint) `notElem` handledWork state
  ]

findSource :: Text -> WorkState value -> WorkSource value
findSource name state = case filter ((== name) . sourceName) (collectedWork state) of
  current : _ -> current
  [] -> WorkSource name (WorkProgress [] []) Nothing WorkOpen Nothing

ensure :: Text -> WorkState value -> WorkState value
ensure name state = putSource (findSource name state) state

putSource :: WorkSource value -> WorkState value -> WorkState value
putSource source state = state { collectedWork =
  if any ((== sourceName source) . sourceName) (collectedWork state)
  then map (\old -> if sourceName old == sourceName source then source else old) (collectedWork state)
  else collectedWork state ++ [source]
  }

-- Use another projection when partial evidence unlocks a known consumer. The
-- default wakes only for question deltas, source failure and terminal results.
notifyWork :: AgentRef -> (WorkEvent value -> Maybe Text) -> WorkSink value
notifyWork owner render = WorkSink $ \policy event -> do
  case workNoticeMessage policy render event of
    Nothing -> pure noWorkDelivery
    Just message -> do
      receipt <- sendMessage owner message
      pure (WorkDelivery (Just receipt) [])

workNoticeMessage
  :: WorkNoticePolicy -> (WorkEvent value -> Maybe Text) -> WorkEvent value -> Maybe Text
workNoticeMessage policy render event =
  combine (render event) (checkpointMessage policy event)

combine :: Maybe Text -> Maybe Text -> Maybe Text
combine Nothing other = other
combine message Nothing = message
combine (Just message) (Just other) = Just (message <> "; " <> other)

checkpointMessage :: WorkNoticePolicy -> WorkEvent value -> Maybe Text
checkpointMessage QuestionsAndResults _ = Nothing
checkpointMessage policy (WorkChanged name delta) = combine reviewed raw
  where
    reviewed = case addedReviewed delta of
      [] -> Nothing
      checkpoints -> Just (name <> ": reviewed checkpoint " <>
        Text.intercalate ", " [renderGitOid (candidateCommit (checkpointCandidate checkpoint))
          <> " (remaining gates: " <> gates (checkpointCandidate checkpoint) <> ")"
        | checkpoint <- checkpoints])
    raw = case policy of
      AllCheckpoints -> case addedEvidence delta of
        [] -> Nothing
        fresh -> Just (name <> ": checkpoint " <>
          Text.intercalate "," (map (renderGitOid . candidateCommit) fresh))
      _ -> Nothing
    gates candidate = case remainingGates candidate of
      [] -> "none reported"
      remaining -> Text.intercalate "; " remaining
checkpointMessage _ _ = Nothing

-- A progress candidate is a checkpoint. Only a terminal result carries the
-- submitted worktree observation needed to identify the exact review source.
data ReviewReadiness
  = ReviewReady Text Candidate
  | ReviewSourceRejected Text Text
  deriving (Show, Eq)

reviewReadiness :: WorkEvent (Outcome Candidate) -> Maybe ReviewReadiness
reviewReadiness (WorkFinished name (Right receipt)) = case responseValue receipt of
  Produced candidate -> Just $ either
    (ReviewSourceRejected name) (ReviewReady name)
    (candidateAtSubmission candidate (responseWorktree receipt))
  Blocked _ _ -> Nothing
reviewReadiness _ = Nothing

reviewReadyMessage :: WorkEvent (Outcome Candidate) -> Maybe Text
reviewReadyMessage event = case reviewReadiness event of
  Just (ReviewReady name candidate) -> Just
    (name <> ": candidate " <> renderGitOid (candidateCommit candidate)
      <> " has a matching submitted HEAD; ready for independent review. Child-reported remaining gates: "
      <> case remainingGates candidate of
        [] -> "none reported"
        gates -> Text.intercalate "; " gates)
  Just (ReviewSourceRejected name reason) -> Just
    (name <> ": candidate source could not be admitted for review: " <> reason)
  Nothing -> Nothing

notifyReviewReady :: AgentRef -> WorkSink (Outcome Candidate)
notifyReviewReady owner = notifyWork owner reviewReadyMessage

workMessage :: (value -> Text) -> WorkEvent value -> Maybe Text
workMessage render event = case event of
  WorkFinished name result -> Just (name <> ": " <> either
    (\failure -> "unavailable " <> Text.pack (show failure)) (render . responseValue) result)
  _ -> workQuestionsMessage event

-- Use when another retained route owns settlement. Questions and failed progress
-- still wake the owner; ordinary evidence and terminal results do not duplicate it.
workQuestionsMessage :: WorkEvent value -> Maybe Text
workQuestionsMessage event = case event of
  WorkChanged name delta ->
    let ref q = questionPlan (questionDetails q) <> "#" <> questionKey q
          <> "@" <> renderGitOid (questionSource (questionDetails q))
        added q = "+" <> ref q <> " " <> questionFinding (questionDetails q)
        removed q = "-" <> ref q
        parts = map added (openedQuestions delta) ++ map removed (resolvedQuestions delta)
    in if null parts then Nothing else Just (name <> ": " <> Text.intercalate "; " parts)
  WorkEnded name (WorkRejected failure) -> Just (name <> ": progress " <> Text.pack (show failure))
  _ -> Nothing

-- A component owner can consume useful partial checkpoints before final delivery.
-- Preserve simultaneous question/result messages instead of choosing one signal.
withCheckpoints :: (WorkEvent value -> Maybe Text) -> WorkEvent value -> Maybe Text
withCheckpoints render event = case (render event, checkpoints event) of
  (Nothing, other) -> other
  (message, Nothing) -> message
  (Just message, Just refs) -> Just (message <> "; " <> refs)
  where
    checkpoints (WorkChanged name delta) =
      case addedEvidence delta of
        [] -> Nothing
        fresh -> Just (name <> ": checkpoint " <> Text.intercalate "," (map (renderGitOid . candidateCommit) fresh))
    checkpoints _ = Nothing
