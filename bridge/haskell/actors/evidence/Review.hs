-- Pure task-level review; no actor authority or runtime state is inferred.
module Review
  ( Checked(..), Finding(..), Verdict(..), assessCheck, reviewCandidate
  , compactReview
  ) where

import Data.Text (Text)
import qualified Data.Text as Text
import Selection
import WaveContract

data Checked = Checked { check :: WaveCheck, selection :: Selection }
  deriving (Show)

data Finding = WrongRevision | MissingIdentity | MissingEvidence | NotDirectExecution
             | NoCompleteSelection | NotExecuted | UnexpectedOutcome | NoChecks
  deriving (Eq, Show)

data Verdict = PassingEvidence | ReproducedFailure | Insufficient [Finding]
  deriving (Eq, Show)

-- DirectExecution means firsthand execution only, not reviewer independence.
-- Reviewer origin/independence must be established outside this predicate.
-- The reviewer supplies the candidate identity, not a display label from a check.
assessCheck :: Text -> Checked -> Verdict
assessCheck revision (Checked c counts)
  | not (null gaps) = Insufficient gaps
  | checkExpectation c == ExpectKnownFailure = ReproducedFailure
  | otherwise = PassingEvidence
  where
    gaps = concat
      [ [WrongRevision | checkedRevision c /= revision]
      , [MissingIdentity | Text.null (Text.strip revision)
          || null (checkedBinaries c)
          || any (Text.null . Text.strip) (checkedBinaries c)]
      , [MissingEvidence | any (Text.null . Text.strip)
          [checkCommand c, checkEvidencePath c]]
      , [NotDirectExecution | checkBasis c /= DirectExecution]
      , [NoCompleteSelection | not (hasExecutedSelection counts)]
      , [NotExecuted | checkOutcome c `elem` [CompileOnly, DidNotExecute]]
      , [UnexpectedOutcome | not (matches (checkExpectation c) (checkOutcome c))]
      ]
    matches ExpectPassing ExecutedPassed = True
    matches ExpectKnownFailure ExecutedFailed = True
    matches _ _ = False

-- Product acceptance cannot be inferred from reproduction or an empty survey.
-- Missing required checks remain the coordinator's explicit acceptance contract.
reviewCandidate :: Text -> [Checked] -> Verdict
reviewCandidate _ [] = Insufficient [NoChecks]
reviewCandidate revision checks
  | not (null gaps) = Insufficient gaps
  | ReproducedFailure `elem` verdicts = ReproducedFailure
  | otherwise = PassingEvidence
  where
    verdicts = map (assessCheck revision) checks
    gaps = concat [reasons | Insufficient reasons <- verdicts]

-- Compact fold retains full records at the caller instead of replacing evidence.
compactReview :: Text -> [Checked] -> (Verdict, Int, Int)
compactReview revision checks =
  (reviewCandidate revision checks, length checks,
   length [() | Checked c _ <- checks,
                checkOutcome c `elem` [ExecutedPassed, ExecutedFailed]])
