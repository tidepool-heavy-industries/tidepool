{-# LANGUAGE OverloadedStrings #-}
-- Actual retained historical evidence, not synthetic check records.
module Main where

import Control.Monad (unless)
import Data.Text (Text)
import Review
import Selection
import WaveContract

historicalRevision :: Text
historicalRevision = "83733e706b5ce33c9cfdfedc75fb0f4e979a2415"

-- Both logs were inspected by the helper implementer. The reviewer execution
-- was reported by the requester; reading its log is not firsthand execution.
record :: EvidenceBasis -> Text -> Checked
record basis path = Checked
  (WaveCheck historicalRevision
    ["/nix/store/nj7qd6d1pjy1v28bh5mniljsxfr9a57v-ghc-native-bignum-9.12.2-with-packages/bin/ghc; 9.12.2"]
    "runghc -Wall -Werror -ihaskell/actors/evidence -iplans/next haskell/actors/evidence/Tests.hs"
    ExecutedPassed ExpectPassing basis path)
  (SelectedCounts 14 14)

-- A coordinator who did not run either command must keep both attributed.
coordinatorSurvey :: [Checked]
coordinatorSurvey =
  [ record AttributedExecution "/tmp/usage-evidence-83733e70.log"
  , record AttributedExecution "/tmp/helper-independent-review-83733e70.log"
  ]

main :: IO ()
main = do
  putStrLn "Historical synthetic-logic evidence only; not this revised candidate or service acceptance:"
  print historicalRevision
  mapM_ (print . checkEvidencePath . check) coordinatorSurvey
  let foldResult = compactReview historicalRevision coordinatorSurvey
      implementerView = assessCheck historicalRevision
        (record DirectExecution "/tmp/usage-evidence-83733e70.log")
  print foldResult
  putStrLn "Implementer's firsthand evidence view (not independent review):"
  print implementerView
  unless (foldResult == (Insufficient [NotDirectExecution, NotDirectExecution], 2, 2)
          && implementerView == PassingEvidence)
    (error "historical survey perspective regression")
