{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeOperators #-}

module Project.ReviewTools (ReviewTools (..), ReviewSubmitInput (..), ReviewSubmitRefusal (..), tools) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Data.Void (absurd)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract (AsServerT, Call, JsonSchema, (:-), tool)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Aeson.Value (ToJSON)
import Tidepool.Agent.Reply
  ( ReplyError, Replies, RequestScope (..), RequestScopeError (..)
  , attemptReply, currentRequest, requestIdNumber, requestReplyOf
  )
import Tidepool.Actors.Worktree (boundWorktree, observeSubmission)
import Tidepool.Effects.Core
  ( BoundWorktree, DirtySummary (..), GitOid, HeadState (..)
  , SubmissionObservation (..), WorkingState (..)
  )
import Tidepool.Worktree (worktreeId)
import Project.Types

data ReviewSubmitInput = ReviewSubmitInput
  { expectedRequestId :: Int
  , expectedCandidateOid :: GitOid
  , submittedChecks :: [Text]
  , submittedRationale :: Text
  } deriving (Show, Generic)

instance FromJSON ReviewSubmitInput
instance JsonSchema ReviewSubmitInput

data ReviewSubmitRefusal
  = NoActiveReview
  | ReviewTypeMismatch
  | ReviewInputShadowed
  | RequestIdMismatch Int
  | CandidateOidMismatch GitOid
  | ReviewCheckoutUnavailable Text
  | ReviewCheckoutHeadMismatch GitOid
  | ReviewCheckoutDirty
  | ReviewReplyRejected Text
  deriving (Show, Generic)

instance ToJSON ReviewSubmitRefusal
instance JsonSchema ReviewSubmitRefusal

data ReviewTools mode = ReviewTools
  { submit_review :: mode :- Call ReviewSubmitInput ReviewSubmitRefusal
  } deriving (Generic)

tools :: (Member Replies effects, Member BoundWorktree effects)
  => ReviewTools (AsServerT (Eff effects))
tools = ReviewTools
  { submit_review = tool
      "Accept the current typed review request after checking its request id, candidate commit, and bound checkout. Supply checks performed now and a rationale."
      submitReview
  }

submitReview :: forall effects. (Member Replies effects, Member BoundWorktree effects)
  => ReviewSubmitInput -> Eff effects ReviewSubmitRefusal
submitReview submission = do
  scope <- (currentRequest :: Eff effects (RequestScope ReviewRequest (Outcome ReviewDecision)))
  case scope of
    RequestUnavailable reason -> pure (scopeRefusal reason)
    RequestActive request current
      | requestIdNumber request /= expectedRequestId submission ->
          pure (RequestIdMismatch (requestIdNumber request))
      | candidateCommit (reviewInput current) /= expectedCandidateOid submission ->
          pure (CandidateOidMismatch (candidateCommit (reviewInput current)))
      | otherwise -> do
          checkout <- boundWorktree
          case checkout of
            Left error -> pure (ReviewCheckoutUnavailable (Text.pack (show error)))
            Right tree -> do
              observed <- observeSubmission (worktreeId tree)
              case observed of
                Left error -> pure (ReviewCheckoutUnavailable (Text.pack (show error)))
                Right state
                  | headOid (submittedHead state) /= expectedCandidateOid submission ->
                      pure (ReviewCheckoutHeadMismatch (headOid (submittedHead state)))
                  | not (cleanCheckout (workingState state)) -> pure ReviewCheckoutDirty
                  | otherwise -> case requestReplyOf scope of
                      Nothing -> pure NoActiveReview
                      Just reply -> do
                        result <- attemptReply reply (Produced (Accepted (ReviewedCandidate
                          (reviewBasis current) (reviewInput current)
                          (submittedChecks submission) (submittedRationale submission))))
                        case result of
                          Left error -> pure (ReviewReplyRejected (Text.pack (show (error :: ReplyError))))
                          Right impossible -> absurd impossible

scopeRefusal :: RequestScopeError -> ReviewSubmitRefusal
scopeRefusal NoCurrentRequest = NoActiveReview
scopeRefusal RequestTypeMismatch = ReviewTypeMismatch
scopeRefusal RequestInputShadowed = ReviewInputShadowed

cleanCheckout :: WorkingState -> Bool
cleanCheckout state = case changes state of
  DirtySummary [] [] [] _ -> case operation state of
    Nothing -> True
    Just _ -> False
  _ -> False
