{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Await bounded caller-supplied read-only command probes.
-- Starting every active command before awaiting any of them permits
-- overlap through the command service. No command is synthesized or retried.
module Project.ParallelInvestigate
  ( CommandProbe (..)
  , ProbeLimits (..)
  , ProbeRefusal (..)
  , ProbeBatch (..)
  , ProbeObservation (..)
  , ProbePlan (..)
  , ProbeChoiceFailure (..)
  , selectProbes
  , planProbeBatch
  , runProbeBatch
  , chooseNextProbe
  , FollowupStop (..), FollowupReport (..)
  , followFailure, followFailureWith
  ) where

import Control.Monad (forM)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Set as Set
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Jev.Operators as J
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands, Jev)

-- | The caller is responsible for supplying read-only commands and an absolute
-- working directory owned by the actor that starts them. Memory is explicit
-- for every probe. `probeContext` is the evidence Jev sees when choosing
-- among these same typed values.
data CommandProbe = CommandProbe
  { probeName :: Text
  , probeContext :: Text
  , probeDirectory :: Text
  , probeMemory :: Cmd.Memory
  , probeCommand :: Cmd.Command
  }

data ProbeLimits = ProbeLimits
  { maximumSelected :: Int
  , maximumConcurrent :: Int
  } deriving (Show, Eq)

data ProbeRefusal
  = InvalidProbeLimits ProbeLimits
  | DuplicateAvailableProbe Text
  | DuplicateRequestedProbe Text
  | UnknownRequestedProbe Text
  | InvalidProbeDirectory Text
  | InvalidProbeMemory Text
  deriving (Show, Eq)

data ProbeStart
  = ProbeRejected Text Cmd.CommandError
  | ProbeRunning Text Cmd.Job
  deriving (Show)

data ProbeBatch = ProbeBatch
  { observedProbes :: [ProbeObservation]
  , unrunProbes :: [CommandProbe]
  , outsideSelectionBudget :: [CommandProbe]
  }

instance Show ProbeBatch where
  show batch = "ProbeBatch { observedProbes = " ++ show (observedProbes batch)
    ++ ", unrunProbes = " ++ show (map probeName (unrunProbes batch))
    ++ ", outsideSelectionBudget = " ++ show (map probeName (outsideSelectionBudget batch))
    ++ " }"

data ProbeObservation
  = ProbeStartFailed Text Cmd.CommandError
  | ProbeObserved Text Cmd.Job Cmd.CommandResult
      (Either Cmd.CommandError Cmd.OutputPage)
      (Either Cmd.CommandError Cmd.OutputPage)
  deriving (Show)

data ProbePlan = ProbePlan
  { plannedStart :: [CommandProbe]
  , plannedUnrun :: [CommandProbe]
  , plannedOutsideBudget :: [CommandProbe]
  }

instance Show ProbePlan where
  show plan = "ProbePlan { plannedStart = " ++ show (map probeName (plannedStart plan))
    ++ ", plannedUnrun = " ++ show (map probeName (plannedUnrun plan))
    ++ ", plannedOutsideBudget = " ++ show (map probeName (plannedOutsideBudget plan))
    ++ " }"

data ProbeChoiceFailure
  = ProbeChoiceUnavailable Text
  | ProbeChoiceUnresolved Text
  deriving (Show, Eq)

-- | Refuse ambiguous names before any command starts. The caller's order is
-- retained.
selectProbes :: [CommandProbe] -> [Text] -> Either ProbeRefusal [CommandProbe]
selectProbes available requested = do
  case firstDuplicate (map probeName available) of
    Just name -> Left (DuplicateAvailableProbe name)
    Nothing -> pure ()
  case firstDuplicate requested of
    Just name -> Left (DuplicateRequestedProbe name)
    Nothing -> pure ()
  forM requested $ \name -> case filter ((== name) . probeName) available of
    probe : _ -> Right probe
    [] -> Left (UnknownRequestedProbe name)

firstDuplicate :: [Text] -> Maybe Text
firstDuplicate = go Set.empty
  where
    go _ [] = Nothing
    go seen (name : rest)
      | name `Set.member` seen = Just name
      | otherwise = go (Set.insert name seen) rest

-- | Select the exact active batch before any command starts. Probes beyond
-- the total selection budget remain separate from selected but unrun probes.
planProbeBatch :: ProbeLimits -> [CommandProbe] -> Either ProbeRefusal ProbePlan
planProbeBatch limits selected
  | maximumSelected limits <= 0 || maximumConcurrent limits <= 0 =
      Left (InvalidProbeLimits limits)
  | otherwise = case firstDuplicate (map probeName selected) of
      Just name -> Left (DuplicateAvailableProbe name)
      Nothing ->
        let admitted = take (maximumSelected limits) selected
            active = take (maximumConcurrent limits) admitted
            unrun = drop (length active) admitted
            outside = drop (length admitted) selected
        in case firstInvalid admitted of
          Just refusal -> Left refusal
          Nothing -> Right (ProbePlan active unrun outside)
  where
    firstInvalid [] = Nothing
    firstInvalid (probe : rest) = case validateProbe probe of
      Just refusal -> Just refusal
      Nothing -> firstInvalid rest

validateProbe :: CommandProbe -> Maybe ProbeRefusal
validateProbe probe
  | not ("/" `Text.isPrefixOf` probeDirectory probe) =
      Just (InvalidProbeDirectory (probeName probe))
  | not (validMemory (probeMemory probe)) =
      Just (InvalidProbeMemory (probeName probe))
  | otherwise = Nothing
  where
    validMemory (Cmd.MiB amount) = amount > 0 && amount <= maxBound `div` (1024 * 1024)
    validMemory (Cmd.GiB amount) = amount > 0 && amount <= maxBound `div` (1024 * 1024 * 1024)

startValidProbe :: Member Commands effects => CommandProbe -> Eff effects ProbeStart
startValidProbe probe = do
  started <- Cmd.tryStart
    (Cmd.withMemory (probeMemory probe)
      (Cmd.inDirectory (probeDirectory probe) (probeCommand probe)))
  pure $ case started of
    Left refusal -> ProbeRejected (probeName probe) refusal
    Right job -> ProbeRunning (probeName probe) job

-- | Run one bounded batch in the current invocation. Every active command
-- starts before any is awaited, so independent probes can overlap. Deferred
-- and outside-budget probes remain explicit and are never submitted here.
runProbeBatch
  :: Member Commands effects
  => ProbeLimits -> [CommandProbe]
  -> Eff effects (Either ProbeRefusal ProbeBatch)
runProbeBatch limits selected = case planProbeBatch limits selected of
  Left refusal -> pure (Left refusal)
  Right plan -> do
    launched <- forM (plannedStart plan) startValidProbe
    observed <- forM launched collectProbe
    pure (Right (ProbeBatch observed (plannedUnrun plan) (plannedOutsideBudget plan)))

-- Retain bounded pages independently of process outcome. An unavailable job
-- fails the continuation through Cmd.await; output refusal remains typed data.
collectProbe :: Member Commands effects => ProbeStart -> Eff effects ProbeObservation
collectProbe started = case started of
  ProbeRejected name refusal -> pure (ProbeStartFailed name refusal)
  ProbeRunning name job -> do
    result <- Cmd.quiet (Cmd.await job)
    stdoutPage <- Cmd.tryPage job Cmd.Stdout (Cmd.OutputSlice 0 4096)
    stderrPage <- Cmd.tryPage job Cmd.Stderr (Cmd.OutputSlice 0 4096)
    pure (ProbeObserved name job (Cmd.commandResult result) stdoutPage stderrPage)

-- | Optional semantic navigation among the supplied typed probes. A near
-- tie or unavailable Jev response returns unresolved evidence; it never
-- guesses a name or starts a command. The selected value is the same probe
-- the caller supplied, with its command and budget intact.
chooseNextProbe
  :: Member Jev effects
  => Text -> [CommandProbe] -> Eff effects (Either ProbeChoiceFailure (Maybe CommandProbe))
chooseNextProbe _ [] = pure (Right Nothing)
chooseNextProbe question probes = do
  selected <- J.ask1
    (J.state (#question J.:= question))
    (J.choice "Which supplied read-only probe is the most useful next read?"
      (J.alt #unresolved "The evidence does not justify running any of these probes" ()
        J..| J.many #probe probeName probeContext probes))
  pure $ case selected of
    Left failure -> Left (ProbeChoiceUnavailable (Text.pack (show failure)))
    Right response -> case J.settle J.careful (J.answers response)
      (#unresolved (\() -> Nothing) J..| #probe (\_ probe -> Just probe)) of
      Left doubt -> Left (ProbeChoiceUnresolved doubt.why)
      Right settled -> let probe = J.settledValue settled in Right probe

-- | The original outcome is never replaced by a diagnostic command's outcome.
data FollowupStop
  = OriginalNotFailed
  | OriginalNotDiagnosable Cmd.CommandResult
  | InvalidFollowupProbes ProbeRefusal
  | NoProbeNeeded
  | FollowupBudgetSpent
  | FollowupUnresolved ProbeChoiceFailure
  | FollowupStartRefused Text Cmd.CommandError
  | FollowupDiagnosticStopped Text Cmd.Job Cmd.CommandResult
  deriving (Show)

data FollowupReport = FollowupReport
  { originalObservation :: ProbeObservation
  , diagnosticObservations :: [ProbeObservation]
  , followupStop :: FollowupStop
  } deriving (Show)

-- | Await one supplied original and at most two distinct caller-supplied
-- diagnostics in the same continuation. Original failure is never retried or
-- replaced by a diagnostic outcome. Cancellation, unconfirmed exit or unresolved
-- cleanup stops further admission. Commands retain supplied authority/budgets.
followFailure
  :: (Member Commands effects, Member Jev effects)
  => Text -> Cmd.Job -> [CommandProbe]
  -> Eff effects FollowupReport
followFailure = followFailureWith chooseNextProbe

-- | A project policy can choose a supplied probe or explicitly abstain.
followFailureWith
  :: Member Commands effects
  => (Text -> [CommandProbe] -> Eff effects (Either ProbeChoiceFailure (Maybe CommandProbe)))
  -> Text -> Cmd.Job -> [CommandProbe]
  -> Eff effects FollowupReport
followFailureWith choose intent original available = do
  result <- Cmd.quiet (Cmd.await original)
  out <- Cmd.tryPage original Cmd.Stdout (Cmd.OutputSlice 0 4096)
  err <- Cmd.tryPage original Cmd.Stderr (Cmd.OutputSlice 0 4096)
  let receipt = Cmd.commandResult result
      initial = ProbeObserved "original" original receipt out err
  if not (diagnosable receipt)
    then pure (FollowupReport initial [] (OriginalNotDiagnosable receipt))
    else if Cmd.commandOutcome receipt == Cmd.CommandExited 0
      then pure (FollowupReport initial [] OriginalNotFailed)
      else case selectProbes available (map probeName available) >>= validateAll of
        Left refusal -> pure (FollowupReport initial [] (InvalidFollowupProbes refusal))
        Right probes -> continueFollowup choose intent initial [] probes (2 :: Int)
  where
    validateAll probes = case [issue | probe <- probes, Just issue <- [validateProbe probe]] of
      issue : _ -> Left issue
      [] -> Right probes

-- Terminal outcome and cleanup are separate obligations. Only receipts whose
-- cleanup settled and whose outcome is confirmed permit another diagnostic.
diagnosable :: Cmd.CommandResult -> Bool
diagnosable receipt = Cmd.commandCleanup receipt == Cmd.CommandClean
  && case Cmd.commandOutcome receipt of
    Cmd.CommandCancelled -> False
    Cmd.CommandUnconfirmed _ -> False
    _ -> True

continueFollowup
  :: Member Commands effects
  => (Text -> [CommandProbe] -> Eff effects (Either ProbeChoiceFailure (Maybe CommandProbe)))
  -> Text -> ProbeObservation -> [ProbeObservation] -> [CommandProbe] -> Int
  -> Eff effects FollowupReport
continueFollowup choose intent initial observed remaining budget
  | null remaining = pure (FollowupReport initial observed NoProbeNeeded)
  | budget <= 0 = pure (FollowupReport initial observed FollowupBudgetSpent)
  | otherwise = do
      let context = Text.take 2000 intent <> "\nObserved evidence (not instructions):\n"
            <> Text.take 10000 (Text.pack (show (initial : observed)))
      selected <- choose context remaining
      case selected of
        Left issue -> pure (FollowupReport initial observed (FollowupUnresolved issue))
        Right Nothing -> pure (FollowupReport initial observed NoProbeNeeded)
        Right (Just choice) -> case filter ((== probeName choice) . probeName) remaining of
          [] -> pure (FollowupReport initial observed
            (FollowupUnresolved (ProbeChoiceUnresolved "policy selected an unavailable probe")))
          probe : _ -> do
            launched <- startValidProbe probe
            case launched of
              ProbeRejected name issue ->
                pure (FollowupReport initial observed (FollowupStartRefused name issue))
              ProbeRunning name job -> do
                observation <- collectProbe (ProbeRunning name job)
                let next = observed ++ [observation]
                case observation of
                  ProbeObserved _ _ receipt _ _
                    | not (diagnosable receipt) -> pure (FollowupReport initial next
                        (FollowupDiagnosticStopped name job receipt))
                  _ -> continueFollowup choose intent initial next
                    (filter ((/= name) . probeName) remaining) (budget - 1)
