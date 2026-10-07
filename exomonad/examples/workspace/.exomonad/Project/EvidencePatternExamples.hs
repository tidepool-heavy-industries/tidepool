{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | Two authored uses of the same pure question. Both ask only over these
-- supplied, bounded values; neither starts a command or reads a checkout.
module Project.EvidencePatternExamples
  ( Diagnostic (..), ReviewSource (..)
  , EvidencePacket
  , commandEvidence, reviewEvidence
  , commandCriteria, reviewCriteria
  , commandState, reviewState
  , commandPacket, reviewPacket
  , runCommandCase, runReviewCase
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Jev.Operators as J
import Jev.Operators (Packet ((:=), (:&)))
import Project.EvidencePattern
import Tidepool.Effects.Core (Jev)

type EvidencePacket a = ("best" J.::= J.Choice (EvidenceAlternatives a))
  J.:& ("coverage" J.::= J.Noul)

data Diagnostic = Diagnostic
  { diagnosticCode :: Text
  , diagnosticSite :: Text
  , diagnosticInterpretation :: Text
  } deriving (Show, Eq)

data ReviewSource = ReviewSource
  { reviewCommit :: Text
  , reviewPath :: Text
  , reviewLines :: Text
  } deriving (Show, Eq)

-- Existing command-diagnostic fixtures with their source addresses. The
-- examples do not claim these snippets came from a newly run command.
commandEvidence :: [Evidence Diagnostic]
commandEvidence =
  [ Evidence "E0061-site" "eb73928 Project/JevChecks.hs lines 37-40"
      (Text.unlines
        [ "error[E0061]: this function takes 2 arguments but 1 argument was supplied"
        , " --> src/main.rs:83:9"
        ])
      (Diagnostic "E0061" "src/main.rs:83:9" "call site has one argument")
  , Evidence "E0061-definition" "eb73928 Project/JevChecks.hs lines 41-43"
      (Text.unlines ["note: function defined here", " --> src/store.rs:59:1"])
      (Diagnostic "E0061" "src/store.rs:59:1" "definition shows the expected signature")
  , Evidence "E0004-site" "eb73928 Project/JevChecks.hs lines 20-23"
      (Text.unlines
        [ "error[E0004]: non-exhaustive patterns: `None` not covered"
        , " --> src/main.rs:136:67"
        ])
      (Diagnostic "E0004" "src/main.rs:136:67" "different compiler error")
  ]

commandCriteria :: EvidenceCriteria
commandCriteria = (defaultCriteria "identify the best first source span to repair the E0061 failed call")
  { usefulWhen = "The excerpt identifies the failing call and location needed to begin the repair."
  , noneWhen = "Every supplied diagnostic is unrelated to the E0061 repair."
  , insufficientWhen = "The retained excerpts do not show a usable failing site or the task is underspecified."
  }

commandPacket :: J.Packet (EvidencePacket Diagnostic) J.Questions
commandPacket =
  #best := evidenceQuestion commandCriteria
    (\row -> "Entry " <> evidenceKey row <> " in " <> J.field #evidence commandState)
    commandEvidence
    :& #coverage := J.noul
      ("Do entries in " <> J.field #evidence commandState
        <> " identify the failing call location and a corresponding definition location? Judge only those entries.")

commandState = J.state
  (#intent := ("Find the first source span to inspect for this E0061 repair" :: Text)
    :& #evidence := sourceRows commandEvidence)

-- This returns the full response, including raw distributions, diagnostics,
-- resolved model and usage. Policy is applied afterwards by the caller.
runCommandCase
  :: Member Jev effects
  => Eff effects (Either (J.JevError J.JevCallError) (J.Response (J.Packet (EvidencePacket Diagnostic) J.Answers)))
runCommandCase = J.ask commandState commandPacket

-- Exact spans already present at the shared package's baseline commit.
reviewEvidence :: [Evidence ReviewSource]
reviewEvidence =
  [ Evidence "budget" "eb73928 Project/RetainedEvidence.hs lines 19-22"
      (Text.unlines
        [ "evidenceBudget :: Int -> Either EvidenceBudgetIssue EvidenceBudget"
        , "evidenceBudget bytes"
        , "  | bytes > 0 && bytes <= 262144 = Right (EvidenceBudget bytes)"
        , "  | otherwise = Left (InvalidEvidenceBudget bytes)"
        ])
      (ReviewSource "eb73928" "Project/RetainedEvidence.hs" "19-22")
  , Evidence "page" "eb73928 Project/RetainedEvidence.hs lines 53-58"
      (Text.unlines
        [ "readStream :: Member Commands effects => Cmd.Job -> Cmd.CommandStream -> Int -> Eff effects StreamEvidence"
        , "readStream job stream limit = go 0 limit []"
        , "  where"
        , "    go offset remaining pages = do"
        , "      let requested = min 65536 remaining"
        , "      response <- Cmd.tryPage job stream (Cmd.OutputSlice offset requested)"
        ])
      (ReviewSource "eb73928" "Project/RetainedEvidence.hs" "53-58")
  , Evidence "result" "eb73928 Project/RetainedEvidence.hs lines 37-42"
      (Text.unlines
        [ "data RetainedEvidence = RetainedEvidence"
        , "  { retainedJob :: Cmd.Job"
        , "  , retainedStatus :: Cmd.CommandStatus"
        , "  , retainedStdout :: StreamEvidence"
        , "  , retainedStderr :: StreamEvidence"
        , "  } deriving (Show, Eq)"
        ])
      (ReviewSource "eb73928" "Project/RetainedEvidence.hs" "37-42")
  ]

reviewCriteria :: EvidenceCriteria
reviewCriteria = (defaultCriteria "choose the strongest supplied source evidence that retained command output reads have a byte bound")
  { usefulWhen = "The source span directly establishes a maximum byte allowance for retained output reads."
  , noneWhen = "The supplied spans are sufficiently described but none establishes a byte bound."
  , insufficientWhen = "The supplied spans omit the budget or read path needed to judge the bound."
  }

reviewPacket :: J.Packet (EvidencePacket ReviewSource) J.Questions
reviewPacket =
  #best := evidenceQuestion reviewCriteria
    (\row -> "Entry " <> evidenceKey row <> " in " <> J.field #evidence reviewState)
    reviewEvidence
    :& #coverage := J.noul
      ("Do entries in " <> J.field #evidence reviewState
        <> " show both the accepted maximum budget and how each page request is limited? Judge only those entries.")

reviewState = J.state
  (#review_goal := ("Inspect retained-output byte bounds at commit eb73928" :: Text)
    :& #evidence := sourceRows reviewEvidence)

sourceRows :: [Evidence a] -> [(Text, [(Text, Text)])]
sourceRows rows =
  [ (evidenceKey row,
      [ ("source", evidenceSource row)
      , ("excerpt", evidenceExcerpt row)
      ])
  | row <- rows
  ]

runReviewCase
  :: Member Jev effects
  => Eff effects (Either (J.JevError J.JevCallError) (J.Response (J.Packet (EvidencePacket ReviewSource) J.Answers)))
runReviewCase = J.ask reviewState reviewPacket
