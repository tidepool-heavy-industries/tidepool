{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

-- Collect answers from supplied requests; the caller owns admission and retirement.
module Project.Interview
  ( InterviewItem (..)
  , InterviewWait (..)
  , InterviewFinding (..)
  , InterviewReport (..)
  , collectInterview
  , interviewComplete
  , interviewSummary
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Actors.Exomonad
import Tidepool.Worktree (renderGitOid)
import Project.Types

data InterviewItem
  = KnownAnswer Question (ResponseResult DesignAnswer)
  | AwaitAnswer Question (Response DesignAnswer)

data InterviewFinding
  = Answered Question (ResponseResult DesignAnswer)
  | Waiting Question InterviewWait
  | AnswerUnavailable Question ResponseFailure
  deriving (Show)

data InterviewWait
  = Queued Text
  | AnswerWorking
  | CancellationPending CancellationReason
  deriving (Show)

newtype InterviewReport = InterviewReport { interviewFindings :: [InterviewFinding] }
  deriving (Show)

-- Existing answers are kept as receipts. Pending handles are observed once;
-- no request is sent and a waiting worker is never treated as having answered.
collectInterview :: Member Replies effects => [InterviewItem] -> Eff effects InterviewReport
collectInterview items = InterviewReport <$> traverse observe items
  where
    observe (KnownAnswer question receipt) = pure (Answered question receipt)
    observe (AwaitAnswer question response) = do
      state <- pollResponse response
      pure $ case state of
        ResponseReady receipt -> Answered question receipt
        ResponseUnavailable failure -> AnswerUnavailable question failure
        ResponseStarting reason -> Waiting question (Queued reason)
        ResponsePending _ -> Waiting question AnswerWorking
        ResponseCancellationPending reason -> Waiting question (CancellationPending reason)

interviewComplete :: InterviewReport -> Bool
interviewComplete (InterviewReport findings) = not (null findings) && all answered findings
  where
    answered (Answered _ _) = True
    answered _ = False

interviewSummary :: InterviewReport -> Text
interviewSummary report@(InterviewReport findings) = Text.unlines $
  (if null findings then "Interview incomplete: no questions supplied"
   else if interviewComplete report then "Interview complete" else "Interview incomplete")
  : map line findings
  where
    line finding = case finding of
      Answered question receipt -> prefix question <> " answered: "
        <> answerSummary (responseValue receipt)
      Waiting question reason -> prefix question <> " waiting: " <> waitSummary reason
      AnswerUnavailable question failure -> prefix question <> " unavailable: "
        <> Text.pack (show failure)
    prefix question = questionKey question <> "@"
      <> renderGitOid (questionSource (questionDetails question))

waitSummary :: InterviewWait -> Text
waitSummary wait = case wait of
  Queued reason -> "queued: " <> reason
  AnswerWorking -> "working"
  CancellationPending reason -> "cancellation pending: " <> Text.pack (show reason)

answerSummary :: DesignAnswer -> Text
answerSummary answer = case answer of
  Decision decision evidence -> "decision " <> decision <> " ("
    <> Text.pack (show (length evidence)) <> " evidence items)"
  AmendPlan amendment -> "amendment " <> renderGitOid (amendmentCommit amendment)
  NeedEvidence evidence -> "needs evidence: " <> Text.intercalate "; " evidence
