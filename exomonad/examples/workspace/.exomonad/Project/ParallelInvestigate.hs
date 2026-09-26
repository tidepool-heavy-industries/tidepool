{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Start a bounded batch of caller-supplied read-only command probes.
-- Starting every selected command before observing any of them permits
-- overlap through the command service. No command is synthesized or retried.
module Project.ParallelInvestigate
  ( CommandProbe (..)
  , ProbeLimits (..)
  , ProbeRefusal (..)
  , ProbeLaunch (..)
  , ProbeStart (..)
  , ProbeObservation (..)
  , ProbePlan (..)
  , ProbeChoiceFailure (..)
  , selectProbes
  , planProbeBatch
  , startProbeBatch
  , observeProbe
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

data ProbeLaunch = ProbeLaunch
  { startedProbes :: [ProbeStart]
  , unrunProbes :: [CommandProbe]
  , outsideSelectionBudget :: [CommandProbe]
  }

instance Show ProbeLaunch where
  show launch = "ProbeLaunch { startedProbes = " ++ show (startedProbes launch)
    ++ ", unrunProbes = " ++ show (map probeName (unrunProbes launch))
    ++ ", outsideSelectionBudget = " ++ show (map probeName (outsideSelectionBudget launch))
    ++ " }"

data ProbeObservation
  = ProbeStartFailed Text Cmd.CommandError
  | ProbeObserved Text Cmd.Job Cmd.CommandStatus
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

-- | Start at most `maximumConcurrent` jobs. Observe the exact returned handles
-- before starting a continuation; a pending result remains a running job.
startProbeBatch
  :: Member Commands effects
  => ProbeLimits -> [CommandProbe]
  -> Eff effects (Either ProbeRefusal ProbeLaunch)
startProbeBatch limits selected = case planProbeBatch limits selected of
  Left refusal -> pure (Left refusal)
  Right plan -> do
    launched <- forM (plannedStart plan) startValidProbe
    pure (Right (ProbeLaunch launched (plannedUnrun plan) (plannedOutsideBudget plan)))

-- | Observe one exact job after retaining its launch. A running status stays
-- running; the owner can revisit that Job without submitting another command.
-- The first page from each stream carries completeness flags. The caller's
-- Observation is passed through unchanged; the underlying Cmd.observe still
-- fails the cell if the retained Job itself is unavailable.
observeProbe
  :: Member Commands effects
  => Cmd.Observation -> ProbeStart -> Eff effects ProbeObservation
observeProbe observation started = case started of
  ProbeRejected name refusal -> pure (ProbeStartFailed name refusal)
  ProbeRunning name job -> do
    state <- Cmd.quiet (Cmd.observe observation job)
    stdoutPage <- Cmd.tryPage job Cmd.Stdout Cmd.OutputBeginning
    stderrPage <- Cmd.tryPage job Cmd.Stderr Cmd.OutputBeginning
    pure (ProbeObserved name job state stdoutPage stderrPage)

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
    Right answer -> case J.settle J.careful answer
      (#unresolved (\() -> Nothing) J..| #probe (\_ probe -> Just probe)) of
      Left doubt -> Left (ProbeChoiceUnresolved doubt.why)
      Right (J.Settled probe) -> Right probe

-- | The original outcome is never replaced by a diagnostic command's outcome.
data FollowupStop
  = OriginalNotFailed
  | OriginalStillRunning
  | OriginalNotDiagnosable Cmd.CommandResult
  | InvalidFollowupProbes ProbeRefusal
  | NoProbeNeeded
  | FollowupBudgetSpent
  | FollowupUnresolved ProbeChoiceFailure
  | FollowupStillRunning Cmd.Job
  | FollowupStartRefused Text Cmd.CommandError
  deriving (Show)

data FollowupReport = FollowupReport
  { originalObservation :: ProbeObservation
  , diagnosticObservations :: [ProbeObservation]
  , followupStop :: FollowupStop
  } deriving (Show)

-- | At most two contextual, caller-supplied read-only diagnostics after a
-- terminal failure. Use from a completion handler or after a terminal observation;
-- a pending original is returned unchanged for the caller's existing completion
-- route. Observation works with shared jobs and never arms another owner's notice.
-- This never
-- retries the original command. Cancellation, unconfirmed exit, or retained
-- cleanup stop before diagnostics. Commands keep their supplied authority/budget.
followFailure
  :: (Member Commands effects, Member Jev effects)
  => Text -> Cmd.Job -> [CommandProbe]
  -> Eff effects FollowupReport
followFailure = followFailureWith chooseNextProbe

-- | Ordinary functions can supply a deterministic project policy instead of
-- Jev. The policy selects an existing probe value or explicitly abstains.
followFailureWith
  :: Member Commands effects
  => (Text -> [CommandProbe] -> Eff effects (Either ProbeChoiceFailure (Maybe CommandProbe)))
  -> Text -> Cmd.Job -> [CommandProbe]
  -> Eff effects FollowupReport
followFailureWith choose intent original available = do
  state <- Cmd.quiet (Cmd.observe (Cmd.Observation 0 0) original)
  out <- Cmd.tryPage original Cmd.Stdout (Cmd.OutputSlice 0 4096)
  err <- Cmd.tryPage original Cmd.Stderr (Cmd.OutputSlice 0 4096)
  let initial = ProbeObserved "original" original state out err
  case state of
    Cmd.CommandFinished receipt
      | Cmd.commandCleanup receipt /= Cmd.CommandClean ->
          pure (FollowupReport initial [] (OriginalNotDiagnosable receipt))
      | Cmd.commandOutcome receipt == Cmd.CommandCancelled ->
          pure (FollowupReport initial [] (OriginalNotDiagnosable receipt))
      | Cmd.CommandUnconfirmed _ <- Cmd.commandOutcome receipt ->
          pure (FollowupReport initial [] (OriginalNotDiagnosable receipt))
      | Cmd.commandOutcome receipt /= Cmd.CommandExited 0 ->
          case selectProbes available (map probeName available) >>= validateAll of
            Left refusal -> pure (FollowupReport initial [] (InvalidFollowupProbes refusal))
            Right probes -> continue initial [] probes (2 :: Int)
    Cmd.CommandFinished _ -> pure (FollowupReport initial [] OriginalNotFailed)
    _ -> pure (FollowupReport initial [] OriginalStillRunning)
  where
    validateAll probes = case [issue | probe <- probes, Just issue <- [validateProbe probe]] of
      issue : _ -> Left issue
      [] -> Right probes
    continue initial observed remaining budget
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
                    state <- Cmd.quiet (Cmd.observeCompletion (Cmd.Observation 30000 0) job)
                    out <- Cmd.tryPage job Cmd.Stdout (Cmd.OutputSlice 0 4096)
                    err <- Cmd.tryPage job Cmd.Stderr (Cmd.OutputSlice 0 4096)
                    let next = observed ++ [ProbeObserved name job state out err]
                    case state of
                      Cmd.CommandFinished _ -> continue initial next
                        (filter ((/= name) . probeName) remaining) (budget - 1)
                      _ -> pure (FollowupReport initial next (FollowupStillRunning job))
