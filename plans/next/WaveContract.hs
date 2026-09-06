-- Task-local delivery contract for this wave, not runtime lifecycle state.
module WaveContract where

import Data.Text (Text)

data CheckOutcome = ExecutedPassed | ExecutedFailed | CompileOnly | DidNotExecute
  deriving (Eq, Show)

data CheckExpectation = ExpectPassing | ExpectKnownFailure
  deriving (Eq, Show)

data EvidenceBasis = DirectExecution | AttributedExecution | SourceInspection
  deriving (Eq, Show)

data WaveCheck = WaveCheck
  { checkedRevision :: Text
  , checkedBinaries :: [Text]
  , checkCommand :: Text
  , checkOutcome :: CheckOutcome
  , checkExpectation :: CheckExpectation
  , checkBasis :: EvidenceBasis
  , checkEvidencePath :: Text
  } deriving (Show)

data WaveDelivery = WaveDelivery
  { candidateCommits :: [Text]
  , integrationBase :: Text
  , deliveryChecks :: [WaveCheck]
  , deliveryFindings :: Text
  , deliveryBlockers :: [Text]
  } deriving (Show)

-- A reporting projection, never a product-acceptance verdict.
executedChecks :: WaveDelivery -> [WaveCheck]
executedChecks = filter executed . deliveryChecks
  where
    executed check = checkOutcome check `elem` [ExecutedPassed, ExecutedFailed]
