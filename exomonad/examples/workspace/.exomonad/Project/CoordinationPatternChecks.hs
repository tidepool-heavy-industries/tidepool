{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Focused deterministic recipe checks. `liveCases` supplies bounded Jev
-- probes; running them requires an installed Jev backend.
module Project.CoordinationPatternChecks (construction, liveCases) where

import Control.Monad.Freer (Eff, Member)
import Data.Either (isRight)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Jev.Operators as J
import Tidepool.Check
import Tidepool.Effects.Core (Jev)
import Project.CoordinationPattern
import Project.CoordinationPatternExamples

construction :: Member RecipeCheck effects => Eff effects ()
construction = do
  let cases = exampleCases
      lookupCase name = [input | (key, _, input, _) <- cases, key == name]
  check "the five bounded probes cover both criteria" (length cases == 5
    && length (lookupCase "review-mixed") == 1
    && length (lookupCase "consumer-changed") == 1)
  check "mixed review retains the new finding beside handled status"
    (case lookupCase "review-mixed" of
      [input] -> length (comparisonIncoming input) == 2
        && length (comparisonHandled input) == 1
        && all (not . Text.null . factSource) (comparisonIncoming input)
      _ -> False)
  check "missing evidence cannot enter a semantic request"
    (case lookupCase "review-missing" of
      [input] -> prepareComparison input == Left (Unresolved "incoming fact lacks source, claim, or evidence")
      _ -> False)
  check "an unreferenced handled claim cannot establish repetition"
    (case lookupCase "review-repeat" of
      [input] -> prepareComparison (input
        { comparisonHandled = [HandledFact "review/abc123#status" "passed" ""] })
        == Left (Unresolved "handled fact lacks source, claim, or incorporation reference")
      _ -> False)
  check "empty updates cannot become repetition"
    (case lookupCase "consumer-changed" of
      [input] -> prepareComparison (input { comparisonIncoming = [] })
        == Left (Unresolved "no incoming facts supplied")
      _ -> False)
  check "the shared packet composes with a local question"
    (case lookupCase "review-mixed" of
      [input] -> isRight (J.request J.jevLatest (comparisonState input)
        (comparisonPacket reviewedCandidateCriteria J.:&
          #needs_source J.:= J.noul "Is another source needed to interpret this review?"))
      _ -> False)

-- | Intended for a bounded live trial, separate from the recipe's pure
-- construction checks. Each response keeps Jev transport, policy doubt, and
-- the full model response distinct for inspection.
liveCases
  :: Member Jev effects
  => Eff effects [(Text, ComparisonResult,
    Either ComparisonResult (Either J.JevError ComparisonRun))]
liveCases = mapM run exampleCases
  where
    run (name, criteria, input, expected) = do
      observed <- assessComparison criteria input
      pure (name, expected, observed)
