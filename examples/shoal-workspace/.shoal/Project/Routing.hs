{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}

-- Project policy for live waves. The kernel owns ordered delivery and lifetime;
-- this actor retains engineering evidence and chooses which changes need judgment.
module Project.Routing
  ( WorkActor (workSnapshot, workNotification, incorporatedWork), WorkState (..), WorkSource (..), WorkStatus (..)
  , WorkEvent (..), WorkDelta (..), workChange, Notice (..), WorkSink
  , followWork, workDefinition, readWork, finishWork, keepWork, outstandingEvidence
  , notifyWork, workMessage, withCheckpoints
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.List (nub, sort, (\\))
import GHC.Generics (Generic)
import qualified Tidepool.Actor.Record as R
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Tidepool.Actor as Actor
import Tidepool.Actors.Shoal
import Tidepool.Effects.Core (Actor)
import Project.Types
import Project.Actors (CoordinationEffects, coordinationActor)
import Project.Work (sameQuestion)

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
  , openedQuestions :: Attention
  , resolvedQuestions :: Attention
  } deriving (Show)

workChange :: Text -> ProgressCursor -> WorkProgress -> WorkProgress -> WorkEvent value
workChange name cursor previous current = WorkChanged name WorkDelta
  { deltaCursor = cursor
  , addedEvidence = workEvidence current \\ workEvidence previous
  , openedQuestions = workQuestions current \\ workQuestions previous
  , resolvedQuestions = [q | q <- workQuestions previous,
      not (any (sameQuestion q) (workQuestions current))]
  }

data WorkEvent value
  = WorkChanged Text WorkDelta
  | WorkEnded Text WorkStatus
  | WorkFinished Text (Either ResponseFailure (ResponseResult value))
  deriving (Show)

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

data WorkActor value mode = WorkActor
  { workState :: mode :- State (WorkState value)
  , workSnapshot :: mode :- Call () (R.Reply (WorkState value))
  , workNotification :: mode :- Call NotificationReceipt (R.Reply (Either NotificationError NotificationState))
  , incorporatedWork :: mode :- Call (Text, [Candidate]) NoReply
  , workUpdates :: mode :- Event (Text, ProgressState WorkProgress)
  , workResults :: mode :- Event (Text, Either ResponseFailure (ResponseResult value))
  } deriving Generic

type WorkEffects value = CoordinationEffects (WorkActor value)
type WorkSink value = WorkEvent value
  -> Handler (WorkState value) (WorkEffects value)
       (Maybe (Either NotificationError NotificationReceipt))

keepWork :: WorkSink value
keepWork _ = pure Nothing

followWork
  :: Member Actor effects
  => [(Text, Response value, Progress WorkProgress)] -> WorkSink value
  -> Eff effects (ActorHandle (WorkActor value))
followWork inputs sink = R.start (workDefinition inputs sink)

readWork :: Member Actor effects => ActorHandle (WorkActor value) -> Eff effects (WorkState value)
readWork router = R.call (workSnapshot (R.client router)) ()

finishWork :: Member Actor effects => ActorHandle (WorkActor value) -> Eff effects (Actor.ActorExit (WorkState value))
finishWork = R.finish

workDefinition
  :: forall value. [(Text, Response value, Progress WorkProgress)] -> WorkSink value
  -> ActorSpec (WorkActor value) (WorkEffects value)
workDefinition inputs sink
  | length names /= length (nub names) = error "work source names must be unique"
  | otherwise = coordinationActor "work" WorkActor
      { workState = WorkState [WorkSource name (WorkProgress [] []) Nothing WorkOpen Nothing | name <- names] [] [] []
      , workSnapshot = \() -> R.get
      , workNotification = pollNotification
      , incorporatedWork = \(name, candidates) -> R.modify' (\state -> state
          { handledWork = nub (handledWork state ++ [(name, candidate) | candidate <- candidates]) })
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
              next = WorkProgress (nub (workEvidence previous ++ workEvidence progress))
                (nub (sort (workQuestions progress)))
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
notifyWork :: MessageRecipient recipient => recipient -> (WorkEvent value -> Maybe Text) -> WorkSink value
notifyWork owner render event = case render event of
  Nothing -> pure Nothing
  Just message -> Just <$> sendMessage owner message

workMessage :: (value -> Text) -> WorkEvent value -> Maybe Text
workMessage render event = case event of
  WorkChanged name delta ->
    let opened = openedQuestions delta
        closed = resolvedQuestions delta
        ref q = questionPlan (questionDetails q) <> "#" <> questionKey q
          <> "@" <> questionSource (questionDetails q)
        added q = "+" <> ref q <> " " <> questionFinding (questionDetails q)
        removed q = "-" <> ref q
        parts = map added opened ++ map removed closed
    in if null parts then Nothing else Just (name <> ": " <> Text.intercalate "; " parts)
  WorkEnded name (WorkRejected failure) -> Just (name <> ": progress " <> shown failure)
  WorkEnded _ _ -> Nothing
  WorkFinished name result -> Just (name <> ": " <> either
    (\failure -> "unavailable " <> shown failure) (render . responseValue) result)
  where
    shown :: Show a => a -> Text
    shown = Text.pack . show

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
        fresh -> Just (name <> ": checkpoint " <> Text.intercalate "," (map candidateCommit fresh))
    checkpoints _ = Nothing
