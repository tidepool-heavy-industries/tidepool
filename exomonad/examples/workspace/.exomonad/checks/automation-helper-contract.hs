{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}

module Main (main) where

import Control.Monad (unless, void)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Jev.Operators as J
import Project.AssumptionWatch (watchIncorporatedBaseline)
import Project.ParallelInvestigate
import Project.SlowCommandWatch (SlowHandler, SlowObservation (..))
import Project.Types (Incorporation)
import Tidepool.Actors.Exomonad (Actor, AgentRef, GitOid, Response)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands, Jev)

-- A parent can retain the pending child's baseline while starting two exact,
-- read-only command probes in its own checkout. The returned launch retains
-- any jobs that need later observation and the probes not yet run.
pendingChildExample
  :: (Member Actor effects, Member Commands effects)
  => AgentRef -> GitOid -> Response Incorporation -> Text
  -> Eff effects (Either ProbeRefusal ProbeLaunch)
pendingChildExample owner baseline incorporation checkout = do
  void (watchIncorporatedBaseline owner baseline "pending child" incorporation)
  let probe name context args =
        CommandProbe name context checkout (Cmd.MiB 128) (Cmd.argv args)
  result <- startProbeBatch (ProbeLimits 2 2)
    [ probe "status" "working tree status" ["git", "status", "--short"]
    , probe "head" "current commit" ["git", "rev-parse", "HEAD"]
    ]
  pure result

-- Run in a later cell after binding the launch, so an unavailable job cannot
-- discard another already-started job handle.
observeOneExample :: Member Commands effects => ProbeStart -> Eff effects ProbeObservation
observeOneExample = observeProbe (Cmd.Observation 0 0)

-- Semantic selection carries the caller's typed command and budget directly
-- into execution; there is no second name lookup or synthesized command.
chosenProbeExample
  :: (Member Jev effects, Member Commands effects)
  => Text -> [CommandProbe]
  -> Eff effects (Either ProbeChoiceFailure (Maybe (Either ProbeRefusal ProbeLaunch)))
chosenProbeExample question probes = do
  chosen <- chooseNextProbe question probes
  traverse (traverse (startProbeBatch (ProbeLimits 1 1) . pure)) chosen

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
assert label passed = unless passed (error label)

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
