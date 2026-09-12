module Fidelity.Introspection (checks) where

import Control.Exception (SomeException, try)
import Data.List (find)
import System.Directory (createDirectoryIfMissing)
import Tidepool.ExtractRequest (InspectionRequest (..))
import Tidepool.GhcPipeline (PipelineResult (..), runPipeline)
import Tidepool.Introspection
  ( Availability (..),
    InfoEntry (..),
    InspectionResult (..),
    TypeMatch (..),
    runInspection,
  )

import Fidelity.Harness (Check, check)

checks :: IO [Check]
checks = do
  let directory = "test-fidelity/work/introspection"
      path = directory ++ "/IntrospectionCase.hs"
  createDirectoryIfMissing True directory
  source <- readFile "test-fidelity/fixtures/IntrospectionCase.hs"
  writeFile path source
  outcome <- try $ do
    compiled <- runPipeline path [directory, "lib"]
    runInspection
      (prHscEnv compiled)
      (prTargetTcGblEnv compiled)
      (prTargetRdrEnv compiled)
      (prCapturedTypes compiled)
      [ InspectTypeSearch "Eff effects ()",
        InspectNameInfo "allowed",
        InspectNameInfo "forbidden",
        InspectNameInfo "fixedWrongRow",
        InspectNameInfo "polymorphic",
        InspectNameInfo "requiresShow",
        InspectTypeOf "allowed"
      ]
  standalone <- standaloneChecks
  pure $ (case outcome of
    Left (failure :: SomeException) -> [check ("inspection compiler and solver: " ++ show failure) False]
    Right results ->
      [ check "constrained lookup retains permitted candidate" (matchAvailability "allowed" results == Just Available),
        check "constrained lookup retains forbidden candidate" (matchAvailability "forbidden" results == Just Unavailable),
        check "named allowed effect is available" (infoAvailabilityFor "allowed" results == Just Available),
        check "named forbidden effect is unavailable" (infoAvailabilityFor "forbidden" results == Just Unavailable),
        check "fixed wrong output row is unavailable" (infoAvailabilityFor "fixedWrongRow" results == Just Unavailable),
        check "unresolved input effect remains unknown" (infoAvailabilityFor "polymorphic" results == Just Unknown),
        check "unresolved non-row predicate remains unknown" (infoAvailabilityFor "requiresShow" results == Just Unknown),
        check "captured type receives availability" (lastTypeAvailability results == Just Available)
      ]) ++ standalone

standaloneChecks :: IO [Check]
standaloneChecks = do
  let directory = "test-fidelity/work/introspection-standalone"
      path = directory ++ "/IntrospectionStandalone.hs"
  createDirectoryIfMissing True directory
  source <- readFile "test-fidelity/fixtures/IntrospectionStandalone.hs"
  writeFile path source
  outcome <- try $ do
    compiled <- runPipeline path [directory, "lib"]
    runInspection
      (prHscEnv compiled)
      (prTargetTcGblEnv compiled)
      (prTargetRdrEnv compiled)
      (prCapturedTypes compiled)
      [InspectNameInfo "allowed", InspectNameInfo "pureValue"]
  pure $ case outcome of
    Left (failure :: SomeException) -> [check ("standalone inspection: " ++ show failure) False]
    Right results ->
      [ check "standalone effect row is unknown" (infoAvailabilityFor "allowed" results == Just Unknown),
        check "standalone pure value remains available" (infoAvailabilityFor "pureValue" results == Just Available)
      ]

matchAvailability :: String -> [InspectionResult] -> Maybe Availability
matchAvailability wanted results = do
  InspectionTypeMatches _ matches <- find isMatches results
  typeMatchAvailability <$> find ((== wanted) . typeMatchName) matches
  where
    isMatches (InspectionTypeMatches _ _) = True
    isMatches _ = False

infoAvailabilityFor :: String -> [InspectionResult] -> Maybe Availability
infoAvailabilityFor wanted results = do
  InspectionInfo _ entries <- find matchesName results
  infoAvailability <$> find ((== wanted) . infoName) entries
  where
    matchesName (InspectionInfo name _) = name == wanted
    matchesName _ = False

lastTypeAvailability :: [InspectionResult] -> Maybe Availability
lastTypeAvailability results = case reverse results of
  InspectionType _ _ availability : _ -> Just availability
  _ -> Nothing
