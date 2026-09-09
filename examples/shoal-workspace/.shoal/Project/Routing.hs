{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}

-- Project policy for live waves. The kernel owns ordered delivery and lifetime;
-- this actor retains engineering evidence and chooses which changes need judgment.
module Project.Routing
  ( WorkInput (WorkSnapshot, WorkNotification), WorkState (..), WorkSource (..), WorkStatus (..)
  , WorkEvent (..), Notice (..), WorkSink
  , followWork, workDefinition, keepWork
  , notifyWork, workMessage, withCheckpoints
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.List (nub, sort, (\\))
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Tidepool.Actor as Actor
import Tidepool.Actors.Shoal
import Tidepool.Effects.Core (Actor)
import Project.Types
import Project.Work (sameQuestion)

-- Closure keeps unanswered questions. The terminal response is independent of
-- progress closure and retains its execution/worktree evidence, including failure.
data WorkStatus = WorkOpen | WorkClosed | WorkRejected ReplyError
  deriving (Show, Eq)

data WorkSource value = WorkSource
  { sourceName :: Text
  , sourceProgress :: WorkProgress
  , sourceStatus :: WorkStatus
  , sourceResult :: Maybe (Either ResponseFailure (ResponseResult value))
  } deriving (Show)

data WorkEvent value
  = WorkChanged Text WorkProgress WorkProgress
  | WorkEnded Text WorkStatus
  | WorkFinished Text (Either ResponseFailure (ResponseResult value))
  deriving (Show)

-- Successful admission receipts and failures are both retained. Nothing here
-- automatically retries an uncertain send or claims model incorporation.
data Notice value = Notice
  { noticeEvent :: WorkEvent value
  , noticeReceipt :: Either NotificationError NotificationReceipt
  }

instance Show value => Show (Notice value) where
  show notice = "Notice " ++ show (noticeEvent notice) ++ " "
    ++ either show (const "NotificationReceipt") (noticeReceipt notice)

data WorkState value = WorkState
  { collectedWork :: [WorkSource value]
  , workNotices :: [Notice value]
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

data WorkInput value result where
  WorkUpdate :: Text -> ProgressState WorkProgress -> WorkInput value ()
  WorkSettled :: Text -> Either ResponseFailure (ResponseResult value) -> WorkInput value ()
  WorkSnapshot :: WorkInput value (WorkState value)
  WorkNotification :: NotificationReceipt -> WorkInput value (Either NotificationError NotificationState)

-- Nothing means no native message (e.g. local retention or a typed cast). A
-- returned send failure is data: it must not unwind the observed source update.
type WorkSink value = WorkEvent value
  -> Eff (Actor.ReadOnlyEffects (WorkInput value))
       (Maybe (Either NotificationError NotificationReceipt))

keepWork :: WorkSink value
keepWork _ = pure Nothing

-- No identifiers are inferred from task prose. Each fixed name belongs to one
-- response/progress pair; the handles remain available for independent inspection.
workSources
  :: [(Text, Response value, Progress WorkProgress)] -> [Actor.Source (WorkInput value)]
workSources sources
  | length names /= length (nub names) = error "work source names must be unique"
  | otherwise = concat
      [ [ Actor.progressSource progress (WorkUpdate name)
        , Actor.settlementSource response (WorkSettled name)
        ]
      | (name, response, progress) <- sources
      ]
  where
    names = [name | (name, _, _) <- sources]

followWork
  :: Member Actor effects
  => [(Text, Response value, Progress WorkProgress)] -> WorkSink value
  -> Eff effects (Actor.ActorRef (WorkInput value) (WorkState value))
followWork sources sink = Actor.startActor (workDefinition sources sink)
  (WorkState [WorkSource name (WorkProgress [] []) WorkOpen Nothing | (name, _, _) <- sources] [])

-- Keep the same ordered sources on replacement. State initialization belongs to
-- startActor; replacing a sink retains all committed questions, results and receipts.
workDefinition
  :: forall value. [(Text, Response value, Progress WorkProgress)] -> WorkSink value
  -> Actor.ActorDefinition (WorkState value) (WorkInput value) (WorkState value)
workDefinition sources sink = Actor.withSources (workSources sources) $
  Actor.stateful "work" Actor.ReadOnly step
  where
    step
      :: WorkState value -> WorkInput value result
      -> Eff (Actor.ReadOnlyEffects (WorkInput value)) (result, WorkState value)
    step state WorkSnapshot = pure (state, state)
    step state (WorkNotification receipt) = do
      observed <- pollNotification receipt
      pure (observed, state)
    step state (WorkUpdate name observation) = case observation of
      ProgressPending -> pure ((), ensure name state)
      ProgressUpdate _ progress -> do
        let current = findSource name state
            previous = sourceProgress current
            next = WorkProgress
              (nub (workEvidence previous ++ workEvidence progress))
              (nub (sort (workQuestions progress)))
            updated = putSource (current { sourceProgress = next }) state
        if next == previous then pure ((), updated)
        else publish (WorkChanged name previous next) updated
      ProgressClosed -> ended name WorkClosed state
      ProgressRejected failure -> ended name (WorkRejected failure) state
    step state (WorkSettled name result) =
      publish (WorkFinished name result)
        (putSource ((findSource name state) { sourceResult = Just result }) state)
    ended name status state =
      let current = findSource name state
          updated = putSource (current { sourceStatus = status }) state
      in if sourceStatus current == status then pure ((), updated)
         else publish (WorkEnded name status) updated
    publish event updated = do
      sent <- sink event
      pure ((), updated { workNotices = case sent of
        Nothing -> workNotices updated
        Just receipt -> workNotices updated ++ [Notice event receipt]
        })

findSource :: Text -> WorkState value -> WorkSource value
findSource name state = case filter ((== name) . sourceName) (collectedWork state) of
  current : _ -> current
  [] -> WorkSource name (WorkProgress [] []) WorkOpen Nothing

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
  WorkChanged name old new ->
    let opened = workQuestions new \\ workQuestions old
        closed = [q | q <- workQuestions old, not (any (sameQuestion q) (workQuestions new))]
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
    checkpoints (WorkChanged name old new) =
      case workEvidence new \\ workEvidence old of
        [] -> Nothing
        fresh -> Just (name <> ": checkpoint " <> Text.intercalate "," (map candidateCommit fresh))
    checkpoints _ = Nothing
