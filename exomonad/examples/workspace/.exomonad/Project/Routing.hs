{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE FlexibleContexts #-}

-- Project policy for live waves. The kernel owns ordered delivery and lifetime;
-- this actor retains engineering evidence and chooses which changes need judgment.
module Project.Routing
  ( WorkActor (workSnapshot, workNotification, incorporatedWork), WorkState (..), WorkSource (..), WorkStatus (..)
  , WorkEvent (..), WorkDelta (..), workChange, Notice (..), WorkSink (..)
  , WorkNoticePolicy (..), WorkPolicyReceipt (..), setWorkNoticePolicy
  , followWork, workDefinition, readWork, finishWork, keepWork, outstandingEvidence, outstandingReviewed
  , notifyWork, workNoticeMessage, workMessage, workQuestionsMessage, withCheckpoints
  , ReviewReadiness (..), reviewReadiness, reviewReadyMessage, notifyReviewReady
  , WorkBranch, WorkBatch (..), workChild, unfoldWork, unfoldWorkWith, finishWorkBatch
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.List (nub, sort, (\\))
import GHC.Generics (Generic)
import qualified Tidepool.Actor.Record as R
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Tidepool.Actor as Actor
import Tidepool.Actors.Exomonad
import Tidepool.Worktree (renderGitOid)
import Tidepool.Effects.Core (Actor)
import Project.Types
import Project.Actors (CoordinationEffects, coordinationActor)
import Project.Work (candidateAtSubmission, sameQuestion)

-- A batch describes only the children admitted now. Later batches are ordinary
-- subsequent notebook calls, and results need not be code candidates.
data WorkBranch effects value = WorkBranch Text
  (Unfold effects (Text, Response value, Progress WorkProgress))

data WorkBatch value = WorkBatch
  { batchMembers :: [(Text, Response value, Progress WorkProgress)]
  , batchRouter :: ActorHandle (WorkActor value)
  }

-- Prepared admission needs the caller's concrete result type.
{-# INLINE workChild #-}
workChild
  :: forall value child input parent. (KnownEffects child, Subset child parent)
  => Text -> Branch child input value -> WorkBranch parent value
workChild name branch = WorkBranch name $
  (\(response, progress) -> (name, response, progress))
    <$> childWithProgress @WorkProgress @value (withReport Silent branch)

-- Validate names before admitting children. The caller remains the requester;
-- the collector observes outcomes without acquiring active-update authority.
unfoldWork
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects,
      Member Actor effects)
  => ForkGroupPath -> [WorkBranch effects value] -> WorkSink value
  -> Eff effects (WorkBatch value)
unfoldWork group branches sink = fst <$> unfoldWorkWith group branches (\_ -> pure (sink, ()))

-- Configure routing from the original admitted handles before attaching the
-- collector. Authored compositions use this seam without readmitting children.
unfoldWorkWith
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects,
      Member Actor effects)
  => ForkGroupPath -> [WorkBranch effects value]
  -> ([(Text, Response value, Progress WorkProgress)] -> Eff effects (WorkSink value, extra))
  -> Eff effects (WorkBatch value, extra)
unfoldWorkWith group branches configure
  | null branches = error "unfoldWork: empty batch"
  | length names /= length (nub names) = error "unfoldWork: duplicate child name"
  | any (Text.null . Text.strip) names = error "unfoldWork: empty child name"
  | otherwise = do
      members <- unfold group (sequenceA [branch | WorkBranch _ branch <- branches])
      (sink, extra) <- configure members
      router <- followWork members sink
      pure (WorkBatch members router, extra)
  where names = [name | WorkBranch name _ <- branches]

-- Closing the collector is distinct from retiring children. Refuse while any
-- original request lacks a terminal observation, leaving its route intact.
finishWorkBatch
  :: Member Actor effects => WorkBatch value
  -> Eff effects (Either [Text] (Actor.ActorExit (WorkState value)))
finishWorkBatch batch = do
  state <- readWork (batchRouter batch)
  case [sourceName source | source <- collectedWork state,
        Nothing <- [sourceResult source]] of
    [] -> Right <$> finishWork (batchRouter batch)
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
  , incorporatedWork :: mode :- Call (Text, [Candidate]) NoReply
  , workPolicy :: mode :- Call WorkNoticePolicy (R.Reply WorkPolicyReceipt)
  , workUpdates :: mode :- Event (Text, ProgressState WorkProgress)
  , workResults :: mode :- Event (Text, Either ResponseFailure (ResponseResult value))
  } deriving Generic

type WorkEffects value = CoordinationEffects (WorkActor value)
-- Keep callback internals behind a named value. Notebook type pins can retain
-- WorkSink without exposing State or the collector's private effect list.
newtype WorkSink value = WorkSink
  { runWorkSink :: WorkEvent value
      -> Handler (WorkState value) (WorkEffects value)
           (Maybe (Either NotificationError NotificationReceipt))
  }

keepWork :: WorkSink value
keepWork = WorkSink (const (pure Nothing))

followWork
  :: Member Actor effects
  => [(Text, Response value, Progress WorkProgress)] -> WorkSink value
  -> Eff effects (ActorHandle (WorkActor value))
followWork inputs sink = R.start (workDefinition inputs sink)

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
  -> ActorSpec (WorkActor value) (WorkEffects value)
workDefinition inputs (WorkSink sink)
  | length names /= length (nub names) = error "work source names must be unique"
  | otherwise = coordinationActor "work" WorkActor
      { workState = WorkState [WorkSource name (WorkProgress [] []) Nothing WorkOpen Nothing | name <- names] [] [] [] QuestionsAndResults
      , workSnapshot = \() -> R.get
      , workNotification = pollNotification
      , incorporatedWork = \(name, candidates) -> R.modify' (\state -> state
          { handledWork = nub (handledWork state ++ [(name, candidate) | candidate <- candidates]) })
      , workPolicy = \policy -> do
          state <- R.get
          let before = workNoticePolicy state
          if before == policy then pure (WorkPolicyReceipt Nothing before policy) else do
            let index = length (workHistory state)
            R.put (state { workNoticePolicy = policy
              , workHistory = workHistory state ++ [WorkPolicyChanged before policy] })
            pure (WorkPolicyReceipt (Just index) before policy)
      , workUpdates = R.on (mconcat [fmap ((,) name) (R.progress updates) | (name, _, updates) <- inputs]) update
      , workResults = R.on (mconcat [fmap ((,) name) (R.settlement response) | (name, response, _) <- inputs]) settled
      }
  where
    names = [name | (name, _, _) <- inputs]
    update (name, observation) = do
      state <- R.get
      case observation of
        ProgressPending -> R.put (ensure name state)
        ProgressUpdate cursor progress -> do
          let current = findSource name state
              previous = sourceProgress current
              next = foldl (flip withReviewedCheckpoint)
                (WorkProgress (nub (workEvidence previous ++ workEvidence progress))
                  (nub (sort (workQuestions progress))))
                (workReviewed previous ++ workReviewed progress)
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
      sent <- sink event
      R.modify' (\current -> current { workNotices = case sent of
        Nothing -> workNotices current
        Just receipt -> workNotices current ++ [Notice index receipt] })

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
notifyWork owner render = WorkSink $ \event -> do
  policy <- R.gets workNoticePolicy
  case workNoticeMessage policy render event of
    Nothing -> pure Nothing
    Just message -> Just <$> sendMessage owner message

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
