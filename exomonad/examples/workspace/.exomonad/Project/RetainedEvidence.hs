{-# LANGUAGE FlexibleContexts #-}

-- | Bounded pages from an existing command job. Reading never submits work.
module Project.RetainedEvidence
  ( EvidenceBudget, evidenceBudget, EvidenceBudgetIssue (..)
  , StreamStop (..), StreamEvidence (..), RetainedEvidence (..), recoverRetained
  ) where

import Control.Monad.Freer (Eff, Member)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands)

newtype EvidenceBudget = EvidenceBudget Int deriving (Show, Eq)

data EvidenceBudgetIssue = InvalidEvidenceBudget Int deriving (Show, Eq)

-- Each stream is bounded independently; one noisy stream cannot consume the
-- other stream's diagnostic allowance.
evidenceBudget :: Int -> Either EvidenceBudgetIssue EvidenceBudget
evidenceBudget bytes
  | bytes > 0 && bytes <= 262144 = Right (EvidenceBudget bytes)
  | otherwise = Left (InvalidEvidenceBudget bytes)

data StreamStop
  = StreamComplete
  | StreamCurrentEnd
  | StreamBudgetReached
  | StreamIncompletePage
  | StreamReadRefused Cmd.CommandError
  deriving (Show, Eq)

data StreamEvidence = StreamEvidence
  { streamPages :: [Cmd.OutputPage]
  , streamStop :: StreamStop
  } deriving (Show, Eq)

data RetainedEvidence = RetainedEvidence
  { retainedJob :: Cmd.Job
  , retainedStatus :: Cmd.CommandStatus
  , retainedStdout :: StreamEvidence
  , retainedStderr :: StreamEvidence
  } deriving (Show, Eq)

-- A raw page carries retention gaps, decode flags, offsets and EOF evidence.
-- The stop value says why this bounded read ended; callers retain both.
recoverRetained :: Member Commands effects => EvidenceBudget -> Cmd.Job -> Eff effects RetainedEvidence
recoverRetained (EvidenceBudget limit) job = do
  status <- Cmd.status job
  stdout <- readStream job Cmd.Stdout limit
  stderr <- readStream job Cmd.Stderr limit
  pure (RetainedEvidence job status stdout stderr)

readStream :: Member Commands effects => Cmd.Job -> Cmd.CommandStream -> Int -> Eff effects StreamEvidence
readStream job stream limit = go 0 limit []
  where
    go offset remaining pages = do
      let requested = min 65536 remaining
      response <- Cmd.tryPage job stream (Cmd.OutputSlice offset requested)
      case response of
        Left refusal -> pure (StreamEvidence (reverse pages) (StreamReadRefused refusal))
        Right page -> do
          let detail = Cmd.pageDetails page
              consumed = Cmd.outputEnd detail - offset
              retained = reverse (page : pages)
          if Cmd.outputStart detail /= offset
              || Cmd.outputLostBytes detail /= 0
              || Cmd.outputLossy detail
              || consumed < 0
              || consumed > requested
            then pure (StreamEvidence retained StreamIncompletePage)
            else if Cmd.outputFinished detail && Cmd.outputEnd detail == Cmd.outputAvailableEnd detail
              then pure (StreamEvidence retained StreamComplete)
              else if consumed == 0 || Cmd.outputEnd detail == Cmd.outputAvailableEnd detail
                then pure (StreamEvidence retained StreamCurrentEnd)
                else if consumed == remaining
                  then pure (StreamEvidence retained StreamBudgetReached)
                  else go (Cmd.outputEnd detail) (remaining - consumed) (page : pages)
