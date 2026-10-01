{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}

module Main (main) where

import Control.Monad (unless, void)
import Control.Monad.Freer (Eff, Member, interpret, interpretM, run, runM)
import qualified Control.Monad.Freer.State as State
import Control.Exception (ErrorCall, Exception, throw, throwIO, try)
import Data.IORef (newIORef, readIORef, writeIORef)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Jev.Operators as J
import Project.AssumptionWatch (watchIncorporatedBaseline)
import Project.ParallelInvestigate
import Project.SlowCommandWatch (SlowHandler, SlowObservation (..))
import Exomonad.Contrib.Types (Incorporation)
import Tidepool.Actors.Exomonad (Actor, AgentRef, GitOid, Response)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands (..), Jev, CommandObservation (..))
import Tidepool.Command.Types (Job (..))

-- A parent can retain the pending child's baseline while running two exact,
-- read-only command probes in its own checkout. The returned batch retains
-- terminal observations and the probes not admitted to this batch.
pendingChildExample
  :: (Member Actor effects, Member Commands effects)
  => AgentRef -> GitOid -> Response Incorporation -> Text
  -> Eff effects (Either ProbeRefusal ProbeBatch)
pendingChildExample owner baseline incorporation checkout = do
  void (watchIncorporatedBaseline owner baseline "pending child" incorporation)
  let probe name context args =
        CommandProbe name context checkout (Cmd.MiB 128) (Cmd.argv args)
  result <- runProbeBatch (ProbeLimits 2 2)
    [ probe "status" "working tree status" ["git", "status", "--short"]
    , probe "head" "current commit" ["git", "rev-parse", "HEAD"]
    ]
  pure result

-- Semantic selection carries the caller's typed command and budget directly
-- into execution; there is no second name lookup or synthesized command.
chosenProbeExample
  :: (Member Jev effects, Member Commands effects)
  => Text -> [CommandProbe]
  -> Eff effects (Either ProbeChoiceFailure (Maybe (Either ProbeRefusal ProbeBatch)))
chosenProbeExample question probes = do
  chosen <- chooseNextProbe question probes
  traverse (traverse (runProbeBatch (ProbeLimits 1 1) . pure)) chosen

-- The actor can form a bounded semantic recommendation without waking its
-- owner to summarize a slow job. Uncertain and unavailable judgments remain
-- explicit text in the one notice; neither branch cancels or retries the job.
semanticSlowDiagnostic :: Text -> SlowObservation -> SlowHandler Text
semanticSlowDiagnostic context observation = do
  let stderrExcerpt = either (Text.pack . show) (Text.take 2000 . Cmd.pageText)
        (observedSlowStderr observation)
  answer <- J.ask1
    (J.state (#task J.:= context
      J.:& #status J.:= Text.pack (show (observedSlowStatus observation))
      J.:& #stderr J.:= stderrExcerpt))
    (J.choice "Which bounded diagnostic recommendation is supported?"
      (J.alt #progress "The output shows work progressing; keep observing this job" "Progress visible"
        J..| J.alt #inspect "The output contains a concrete failure or blocked step; inspect its retained log" "Inspect retained log"
        J..| J.alt #unclear "The bounded output does not establish a cause" "Cause unresolved"))
  pure $ case answer of
    Left issue -> "Semantic diagnosis unavailable: " <> Text.pack (show issue)
    Right choice -> case J.takenUnder J.careful choice of
      Left doubt -> "Semantic diagnosis uncertain: " <> doubt.why
      Right (J.Settled recommendation) -> recommendation

assert :: String -> Bool -> IO ()
assert label passed = do
  unless passed (error label)
  putStrLn ("passed: " ++ label)

main :: IO ()
main = do
  let probe name = CommandProbe name name "/tmp" (Cmd.MiB 64) (Cmd.argv ["true"])
      available = map probe ["one", "two", "three"]
      names plan = map probeName (plannedStart plan)
  assert "refuse duplicate available names"
    (case selectProbes [probe "one", probe "one"] ["one"] of
      Left (DuplicateAvailableProbe "one") -> True
      _ -> False)
  assert "refuse duplicate requested names"
    (case selectProbes available ["one", "one"] of
      Left (DuplicateRequestedProbe "one") -> True
      _ -> False)
  assert "refuse unknown requested names"
    (case selectProbes available ["missing"] of
      Left (UnknownRequestedProbe "missing") -> True
      _ -> False)
  assert "cap active and total selected probes separately"
    (case planProbeBatch (ProbeLimits 2 1) available of
      Right plan -> names plan == ["one"] && map probeName (plannedUnrun plan) == ["two"]
        && map probeName (plannedOutsideBudget plan) == ["three"]
      _ -> False)
  assert "deferred probes retain their typed command for the next batch"
    (case planProbeBatch (ProbeLimits 2 1) available of
      Right firstPlan -> case planProbeBatch (ProbeLimits 1 1) (plannedUnrun firstPlan) of
        Right nextPlan -> names nextPlan == ["two"]
        _ -> False
      _ -> False)
  assert "refuse implicit working directory"
    (case planProbeBatch (ProbeLimits 1 1) [(probe "one") { probeDirectory = "relative" }] of
      Left (InvalidProbeDirectory "one") -> True
      _ -> False)
  assert "refuse invalid memory before starting"
    (case planProbeBatch (ProbeLimits 1 1) [(probe "one") { probeMemory = Cmd.MiB 0 }] of
      Left (InvalidProbeMemory "one") -> True
      _ -> False)
  assert "refuse invalid selected but deferred probe before starting any job"
    (case planProbeBatch (ProbeLimits 2 1)
      [probe "one", (probe "two") { probeMemory = Cmd.MiB 0 }] of
      Left (InvalidProbeMemory "two") -> True
      _ -> False)

  nativeContracts

-- Interpret the production Eff program with the real generated Commands GADT.
-- A closed trace records admission, exact identities and bounded output reads;
-- unexpected background, control or bounded-observation effects fail the test.
data Trace
  = Started Cmd.CommandSpec
  | Waited Text
  | ReadPage Text Cmd.CommandStream Cmd.CommandPosition
  deriving (Eq, Show)

-- Fixture/protocol failures must never masquerade as Cmd.await's checked error.
data ProtocolFailure
  = UnscriptedAdmission
  | UnknownJobIdentity Text
  | UnexpectedFiniteEffect
  deriving (Eq, Show)

instance Exception ProtocolFailure

data Script = Script
  { scriptedStarts :: [Either Cmd.CommandError Text]
  , scriptedReceipts :: [(Text, Either Cmd.CommandError Cmd.CommandResult)]
  , scriptedReads :: [(Text, Cmd.CommandStream, Cmd.CommandError)]
  }

data Service = Service Script [Trace]

runCommands :: Script -> Eff '[Commands, State.State Service] a -> (a, [Trace])
runCommands script action =
  let (result, Service _ trace) = run (State.runState (Service script []) (interpret handle action))
  in (result, reverse trace)
  where
    record event = State.modify (\(Service current events) -> Service current (event : events))
    handle :: Commands value -> Eff '[State.State Service] value
    handle request = case request of
      CommandStartWith spec -> do
        record (Started spec)
        Service current events <- State.get
        case scriptedStarts current of
          [] -> throw UnscriptedAdmission
          answer : rest -> do
            State.put (Service current {scriptedStarts = rest} events)
            pure answer
      CommandWaitWith key -> do
        record (Waited key)
        Service current _ <- State.get
        let receipt = case lookup key (scriptedReceipts current) of
              Just answer -> answer
              Nothing -> throw (UnknownJobIdentity key)
        pure (fmap (\result -> CommandObservation result
          (Left (Cmd.CommandUnavailable "capture transport unavailable"))) receipt)
      CommandReadWith key stream position -> do
        record (ReadPage key stream position)
        Service current _ <- State.get
        pure $ case [issue | (job, selected, issue) <- scriptedReads current,
                           job == key, selected == stream] of
          issue : _ -> Left issue
          [] -> Right (Cmd.CommandPage key 0 (Text.length key) (Text.length key)
            0 0 True False False False)
      CommandPresentWith _ _ -> pure ()
      _ -> throw UnexpectedFiniteEffect

receipt :: Cmd.CommandOutcome -> Cmd.CommandCleanup -> Cmd.CommandResult
receipt = Cmd.CommandResult

clean :: Int -> Cmd.CommandResult
clean code = receipt (Cmd.CommandExited code) Cmd.CommandClean

fixture :: Text -> CommandProbe
fixture name = CommandProbe name name "/owned/checkout" (Cmd.MiB 64)
  (Cmd.withEnvironment [("FIXTURE", name)] (Cmd.argv ["printf", name]))

pick :: Text -> [CommandProbe] -> Eff effects (Either ProbeChoiceFailure (Maybe CommandProbe))
pick _ probes = pure (Right (case probes of { [] -> Nothing; first : _ -> Just first }))

starts :: [Trace] -> [Cmd.CommandSpec]
starts trace = [spec | Started spec <- trace]

waits :: [Trace] -> [Text]
waits trace = [key | Waited key <- trace]

nativeContracts :: IO ()
nativeContracts = do
  let oneProbe = fixture "one"
      twoProbe = fixture "two"
      probes = [oneProbe, twoProbe, fixture "three", fixture "four"]
      script = Script [Right "job-one", Right "job-two"]
        [("original", Right (clean 7)), ("job-one", Right (clean 0)), ("job-two", Right (clean 3))] []
      (batch, batchTrace) = runCommands script (runProbeBatch (ProbeLimits 3 2) probes)
      expected probe = Cmd.describe (Cmd.withMemory (probeMemory probe)
        (Cmd.inDirectory (probeDirectory probe) (probeCommand probe)))
  assert "native batch starts all active supplied commands before its first await"
    (take 3 batchTrace == [Started (expected (oneProbe)), Started (expected (twoProbe)), Waited "job-one"])
  assert "native batch preserves exact argv, environment, directory and memory"
    (starts batchTrace == map expected (take 2 probes))
  assert "native batch awaits exact jobs and preserves failure beside deferred budgets"
    (waits batchTrace == ["job-one", "job-two"] && case batch of
      Right result -> map probeName (unrunProbes result) == ["three"]
        && map probeName (outsideSelectionBudget result) == ["four"]
        && case observedProbes result of
          [ProbeObserved "one" (Job "job-one") one _ _, ProbeObserved "two" (Job "job-two") two _ _] -> one == clean 0 && two == clean 3
          _ -> False
      _ -> False)
  assert "native output reads are explicitly bounded per stream"
    (all (\event -> case event of { ReadPage _ _ position -> position == Cmd.OutputSlice 0 4096; _ -> True }) batchTrace)
  let invalid = (twoProbe) {probeMemory = Cmd.MiB 0}
      (invalidBatch, invalidTrace) = runCommands script (runProbeBatch (ProbeLimits 2 1) [oneProbe, invalid])
  assert "native deferred validation refuses the whole batch before any effect"
    (null invalidTrace && case invalidBatch of { Left (InvalidProbeMemory "two") -> True; _ -> False })
  let mixed = script {scriptedStarts = [Left Cmd.CommandUnauthorized, Right "job-two"],
                     scriptedReads = [("job-two", Cmd.Stderr, Cmd.CommandOutputPending)]}
      (mixedBatch, mixedTrace) = runCommands mixed (runProbeBatch (ProbeLimits 2 2) (take 2 probes))
  assert "native batch retains individual admission and output refusals"
    (waits mixedTrace == ["job-two"] && case mixedBatch of
      Right result -> case observedProbes result of
        [ProbeStartFailed "one" Cmd.CommandUnauthorized,
         ProbeObserved "two" (Job "job-two") result (Right _) (Left Cmd.CommandOutputPending)] -> result == clean 3
        _ -> False
      _ -> False)
  let original = Job "original"
      (report, trace) = runCommands script (followFailureWith pick "failure" original (take 3 probes))
  assert "native followup keeps exact original failure and two distinct diagnostic jobs"
    (waits trace == ["original", "job-one", "job-two"] && length (starts trace) == 2 && case report of
      FollowupReport (ProbeObserved "original" job outcome _ _)
        [ProbeObserved "one" first _ _ _, ProbeObserved "two" second _ _ _] FollowupBudgetSpent ->
          job == original && outcome == clean 7 && first == Job "job-one" && second == Job "job-two"
      _ -> False)
  let altered _ remaining = pure (Right (case remaining of
        [] -> Nothing
        first : _ -> Just first {probeDirectory = "/forged", probeMemory = Cmd.MiB 1, probeCommand = Cmd.argv ["forged"]}))
      (_, canonicalTrace) = runCommands script (followFailureWith altered "failure" original [oneProbe])
  assert "native policy cannot alter a selected supplied command or authority"
    (starts canonicalTrace == [expected (oneProbe)])
  let (none, noneTrace) = runCommands script
        (followFailureWith (\_ _ -> pure (Right Nothing)) "abstain" original probes)
  assert "native abstention admits no diagnostic"
    (null (starts noneTrace) && null (diagnosticObservations none)
      && case followupStop none of { NoProbeNeeded -> True; _ -> False })
  let (duplicate, duplicateTrace) = runCommands script
        (followFailureWith pick "duplicate" original [oneProbe, oneProbe])
  assert "native duplicate validation admits no diagnostic"
    (null (starts duplicateTrace) && case followupStop duplicate of
      InvalidFollowupProbes (DuplicateAvailableProbe "one") -> True
      _ -> False)
  let (unavailable, unavailableTrace) = runCommands script
        (followFailureWith (\_ _ -> pure (Left (ProbeChoiceUnavailable "offline"))) "unavailable" original probes)
  assert "native semantic refusal admits no diagnostic"
    (null (starts unavailableTrace) && case followupStop unavailable of
      FollowupUnresolved (ProbeChoiceUnavailable "offline") -> True
      _ -> False)
  let refusedScript = script {scriptedStarts = [Left Cmd.CommandUnauthorized]}
      (refused, refusedTrace) = runCommands refusedScript
        (followFailureWith pick "refused" original probes)
  assert "native start refusal stops without replacing original outcome"
    (length (starts refusedTrace) == 1 && waits refusedTrace == ["original"]
      && case followupStop refused of { FollowupStartRefused "one" Cmd.CommandUnauthorized -> True; _ -> False })
  let unsafe = [receipt Cmd.CommandCancelled Cmd.CommandClean,
                receipt (Cmd.CommandUnconfirmed "exit unknown") Cmd.CommandClean,
                receipt (Cmd.CommandExited 7) Cmd.CommandRetained,
                receipt (Cmd.CommandExited 7) (Cmd.CommandCleanupUnknown "cleanup unknown")]
  mapM_ (\terminal -> do
    let unsafeOriginal = script {scriptedReceipts = [("original", Right terminal)]}
        (stopped, stoppedTrace) = runCommands unsafeOriginal
          (followFailureWith (\_ _ -> error "unsafe original must not call policy") "unsafe original" original probes)
    assert "native unsafe original prevents policy and diagnostic admission"
      (null (starts stoppedTrace) && case followupStop stopped of
        OriginalNotDiagnosable retained -> retained == terminal
        _ -> False)
    let unsafeDiagnostic = script {scriptedReceipts = [("original", Right (clean 7)), ("job-one", Right terminal)]}
        (diagnostic, diagnosticTrace) = runCommands unsafeDiagnostic
          (followFailureWith pick "unsafe diagnostic" original probes)
    assert "native unsafe diagnostic prevents subsequent admission"
      (length (starts diagnosticTrace) == 1 && case followupStop diagnostic of
        FollowupDiagnosticStopped "one" job retained -> job == Job "job-one" && retained == terminal
        _ -> False)) unsafe
  let (unknown, unknownTrace) = runCommands script
        (followFailureWith (\_ _ -> pure (Right (Just (fixture "unknown")))) "unknown selection" original probes)
  assert "native unavailable policy selection admits no command"
    (null (starts unknownTrace) && case followupStop unknown of
      FollowupUnresolved (ProbeChoiceUnresolved _) -> True
      _ -> False)
  let (repeated, repeatedTrace) = runCommands script
        (followFailureWith (\_ _ -> pure (Right (Just oneProbe))) "repeated selection" original probes)
  assert "native policy cannot repeat an already admitted probe"
    (length (starts repeatedTrace) == 1 && length (diagnosticObservations repeated) == 1
      && case followupStop repeated of { FollowupUnresolved (ProbeChoiceUnresolved _) -> True; _ -> False })
  let successful = script {scriptedReceipts = [("original", Right (clean 0))]}
      (success, successTrace) = runCommands successful
        (followFailureWith (\_ _ -> error "successful original must not call policy") "success" original probes)
  assert "native successful original bypasses policy and diagnostics"
    (null (starts successTrace) && case followupStop success of { OriginalNotFailed -> True; _ -> False })
  -- Mutable handler evidence survives the production checked-command error.
  -- The refusal is supplied by the handler, never synthesized by a workflow copy.
  refusalEvidence <- newIORef ([], Nothing)
  let missingIssue = Cmd.CommandUnavailable "missing"
      missingHandler :: Commands value -> IO value
      missingHandler request = case request of
        CommandWaitWith key -> do
          (prior, _) <- readIORef refusalEvidence
          if not (null prior) then throwIO UnexpectedFiniteEffect else pure ()
          if key /= "original" then throwIO (UnknownJobIdentity key) else pure ()
          writeIORef refusalEvidence ([Waited key], Just (key, missingIssue))
          pure (Left missingIssue)
        _ -> throwIO UnexpectedFiniteEffect
  failed <- try (runM (interpretM missingHandler
    (followFailureWith pick "missing original" original probes)))
    :: IO (Either ErrorCall FollowupReport)
  assert "native unavailable original fails the cell rather than synthesizing a result"
    (case failed of { Left _ -> True; Right _ -> False })
  observedRefusal <- readIORef refusalEvidence
  assert "native await failure reached the exact original refusal without downstream effects"
    (observedRefusal == ([Waited "original"], Just ("original", missingIssue)))
  protocol <- try (try (runM (interpretM missingHandler (Cmd.tryStart (Cmd.argv ["true"]))))
    :: IO (Either ErrorCall (Either Cmd.CommandError Cmd.Job)))
    :: IO (Either ProtocolFailure (Either ErrorCall (Either Cmd.CommandError Cmd.Job)))
  assert "native protocol failures escape the checked-command ErrorCall catch"
    (case protocol of { Left UnexpectedFiniteEffect -> True; _ -> False })
