{-# LANGUAGE OverloadedStrings #-}
-- Synthetic logic checks only: these do not exercise a backend or service.
module Main where
import Control.Monad (unless)
import Review
import Selection
import WaveContract

main :: IO ()
main = do
  let good = WaveCheck "rev" ["binary-sha"] "command" ExecutedPassed ExpectPassing DirectExecution "artifact"
      assess c s = assessCheck "rev" (Checked c s)
      full = SelectedCounts 2 2
      -- Firsthand implementer execution passes the evidence predicate; it
      -- supplies no reviewer-origin or independence proof. Attribution fails
      -- for lack of direct execution, not for lack of independent review.
      cases =
        [ assess good full == PassingEvidence
        , assess (good { checkOutcome = ExecutedFailed, checkExpectation = ExpectKnownFailure }) full == ReproducedFailure
        , assess (good { checkedRevision = "old" }) full == Insufficient [WrongRevision]
        , assess (good { checkBasis = AttributedExecution }) full == Insufficient [NotDirectExecution]
        , assess (good { checkBasis = SourceInspection }) full == Insufficient [NotDirectExecution]
        , assess (good { checkOutcome = CompileOnly }) full == Insufficient [NotExecuted, UnexpectedOutcome]
        , assess (good { checkOutcome = DidNotExecute }) full == Insufficient [NotExecuted, UnexpectedOutcome]
        , assess good SelectionUnknown == Insufficient [NoCompleteSelection]
        , all (not . hasExecutedSelection) [SelectedCounts 0 0, SelectedCounts 2 1, SelectedCounts 1 2, SelectedCounts (-1) (-1)]
        , assess (good { checkedBinaries = [] }) full == Insufficient [MissingIdentity]
        , assess (good { checkEvidencePath = " " }) full == Insufficient [MissingEvidence]
        , reviewCandidate "rev" [] == Insufficient [NoChecks]
        , compactReview "rev" [Checked good full] == (PassingEvidence, 1, 1)
        , reviewCandidate "rev" [Checked good full, Checked (good {checkOutcome = ExecutedFailed, checkExpectation = ExpectKnownFailure}) full] == ReproducedFailure
        ]
  unless (and cases) (error ("failed synthetic cases: " ++ show [i | (i, False) <- zip [1 :: Int ..] cases]))
  putStrLn (show (length cases) ++ " synthetic evidence logic checks passed")
