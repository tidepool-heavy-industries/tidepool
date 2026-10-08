{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Two clients of the same comparison question. They supply the facts and
-- their own criterion; the question constructor has no effect or delivery.
module Project.CoordinationPatternExamples
  ( reviewedCandidateInput, consumerCheckpointInput
  , reviewedCandidateCriteria, consumerCheckpointCriteria
  , ComparisonPacket, ComparisonRun (..), assessComparison
  , assessReviewedCandidate, assessConsumerCheckpoint
  , exampleCases
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Jev.Operators as J
import Tidepool.Effects.Core (Jev)
import Project.CoordinationPattern

-- | The original response remains available for distributions, model, usage,
-- and diagnostics. A policy doubt stays separate from Jev transport failure.
data ComparisonRun = ComparisonRun
  { runResponse :: J.Response (ComparisonPacket J.Answers)
  , runVerdict :: Either J.Doubt (J.Settled J.Careful ComparisonResult)
  }

instance Show ComparisonRun where
  show run = "ComparisonRun { model = " <> show (J.resolvedModel (runResponse run))
    <> ", usage = " <> show (J.usage (runResponse run))
    <> ", verdict = " <> (case runVerdict run of
      Left doubt -> show doubt <> " }"
      Right settled -> let verdict = J.settledValue settled in show verdict <> " }")

reviewedCandidateInput
  :: Text -> Text -> [SourceFact] -> [HandledFact] -> ComparisonInput
reviewedCandidateInput candidate episode incoming handled = ComparisonInput
  { comparisonTask = "Decide whether review update for exact candidate " <> candidate
      <> " carries an unincorporated finding or question for the owner."
  , comparisonEpisode = episode
  , comparisonIncoming = incoming
  , comparisonHandled = handled
  }

reviewedCandidateCriteria :: ComparisonCriteria
reviewedCandidateCriteria = ComparisonCriteria
  { attentionCriterion = "A new finding, changed check claim, contradiction, or unresolved question about the exact reviewed candidate requires owner attention, even beside repeated status. Distinguish a later revision or review from the same candidate."
  , repetitionCriterion = "The same candidate's review findings, check claims, and questions are each matched by an explicit prior incorporation reference; no changed claim or unresolved question remains."
  }

consumerCheckpointInput
  :: Text -> Text -> Text -> [SourceFact] -> [HandledFact] -> ComparisonInput
consumerCheckpointInput sourceCommit consumer episode incoming handled = ComparisonInput
  { comparisonTask = "Decide whether checkpoint for agreed production consumer " <> consumer
      <> " at source " <> sourceCommit <> " changes the owner's integration work."
  , comparisonEpisode = episode
  , comparisonIncoming = incoming
  , comparisonHandled = handled
  }

consumerCheckpointCriteria :: ComparisonCriteria
consumerCheckpointCriteria = ComparisonCriteria
  { attentionCriterion = "A source-identified checkpoint establishes a changed production consumer, failed or absent required check, or new integration constraint affecting the agreed consumer. A routine progress count does not establish applicability."
  , repetitionCriterion = "Every checkpoint fact about the agreed consumer and required checks was already explicitly incorporated, with matching source and reference; no new constraint or failing result remains."
  }

assessReviewedCandidate
  :: Member Jev effects
  => Text -> Text -> [SourceFact] -> [HandledFact]
  -> Eff effects (Either ComparisonResult (Either (J.JevError J.JevCallError) ComparisonRun))
assessReviewedCandidate candidate episode incoming handled =
  assessComparison reviewedCandidateCriteria (reviewedCandidateInput candidate episode incoming handled)

assessConsumerCheckpoint
  :: Member Jev effects
  => Text -> Text -> Text -> [SourceFact] -> [HandledFact]
  -> Eff effects (Either ComparisonResult (Either (J.JevError J.JevCallError) ComparisonRun))
assessConsumerCheckpoint sourceCommit consumer episode incoming handled =
  assessComparison consumerCheckpointCriteria (consumerCheckpointInput sourceCommit consumer episode incoming handled)

assessComparison
  :: Member Jev effects => ComparisonCriteria -> ComparisonInput
  -> Eff effects (Either ComparisonResult (Either (J.JevError J.JevCallError) ComparisonRun))
assessComparison criteria input = case prepareComparison input of
  Left unresolved -> pure (Left unresolved)
  Right ready -> do
    response <- J.ask (comparisonState ready) (comparisonPacket criteria)
    pure (Right (fmap (\full -> ComparisonRun full
      (settleComparison J.careful (J.answers full).update)) response))

-- | Bounded live probes for an operator to run with Jev. These include a
-- mixed stale/new update, exact repetition, missing evidence, a changed
-- production consumer, and silence without a consumer checkpoint.
exampleCases :: [(Text, ComparisonCriteria, ComparisonInput, ComparisonResult)]
exampleCases =
  [ ( "review-mixed", reviewedCandidateCriteria
    , reviewedCandidateInput "abc123" "review:7"
        [ SourceFact "review/abc123#status" "The candidate passed the earlier focused check" "review note 4"
        , SourceFact "review/abc123#finding-2" "Retry cancellation still leaks a child process" "review note 7, src/worker.rs:88"
        ]
        [ HandledFact "review/abc123#status" "The candidate passed the earlier focused check" "owner journal 5" ]
    , Attention )
  , ( "review-repeat", reviewedCandidateCriteria
    , reviewedCandidateInput "abc123" "review:8"
        [ SourceFact "review/abc123#status" "The candidate passed the earlier focused check" "review note 4" ]
        [ HandledFact "review/abc123#status" "The candidate passed the earlier focused check" "owner journal 5" ]
    , Repetition )
  , ( "review-missing", reviewedCandidateCriteria
    , reviewedCandidateInput "abc123" "review:9"
        [ SourceFact "review/abc123#finding-3" "A later test reportedly failed" "" ] []
    , Unresolved "incoming fact lacks source, claim, or evidence" )
  , ( "consumer-changed", consumerCheckpointCriteria
    , consumerCheckpointInput "def456" "Exomonad.Contrib.Routing.followWork" "checkpoint:4"
        [ SourceFact "src/Project/Routing.hs@def456" "Production consumer now invokes the new update path; its focused check fails on missing source identity" "compiler output and routing check 4" ] []
    , Attention )
  , ( "consumer-silence", consumerCheckpointCriteria
    , consumerCheckpointInput "def456" "Exomonad.Contrib.Routing.followWork" "checkpoint:5"
        [ SourceFact "status/worker-2" "No worker update has arrived for ten minutes; consumer effect is unknown" "elapsed-time observation only" ] []
    , Unresolved "evidence insufficient to compare update" )
  ]
