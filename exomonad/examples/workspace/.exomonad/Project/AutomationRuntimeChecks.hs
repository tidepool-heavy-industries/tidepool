{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Focused resident checks of actual command-job and record-actor custody.
module Project.AutomationRuntimeChecks (commandCustody) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check

commandCustody :: Member RecipeCheck effects => Eff effects ()
commandCustody = do
  owner <- root
  void $ turn owner $ Text.unlines
    [ "import qualified Tidepool.Command as Cmd"
    , "import qualified Tidepool.Actor.Record as R"
    , "import Project.ParallelInvestigate"
    , "import Project.SlowCommandWatch"
    , "let probe name command = CommandProbe name name \"/tmp\" (Cmd.MiB 64) (Cmd.argv [\"sh\",\"-c\",command])"
    , "let commandProbes = [probe \"first\" \"printf first\", probe \"second\" \"printf second\"]"
    , "launched <- startProbeBatch (ProbeLimits 2 2) commandProbes"
    ]
  started <- turn owner "inspectFull launched"
  check "both supplied command probes start with exact job handles"
    (all (`Text.isInfixOf` output started) ["ProbeRunning \"first\"", "ProbeRunning \"second\""])
  first <- turn owner "let Right batch = launched\nfirstObserved <- observeProbe (Cmd.Observation 1000 0) (head (startedProbes batch))\ninspectFull firstObserved"
  check "first retained probe yields its own stdout page"
    ("ProbeObserved \"first\"" `Text.isInfixOf` output first
      && "first" `Text.isInfixOf` output first)
  second <- turn owner "let Right batch = launched\nsecondObserved <- observeProbe (Cmd.Observation 1000 0) (startedProbes batch !! 1)\ninspectFull secondObserved"
  check "second retained probe stays independently observable"
    ("ProbeObserved \"second\"" `Text.isInfixOf` output second
      && "second" `Text.isInfixOf` output second)
  void $ turn owner "slowJob <- Cmd.background (Cmd.withStdin (Cmd.argv [\"sh\",\"-c\",\"read line; printf done\"]))\nRight slowWatcher <- watchSlowCommand me \"owned slow probe\" slowJob 10 64 (\\observation -> pure (either (const \"stdout unavailable\") Cmd.pageText (observedSlowStdout observation) <> either (const \"stderr unavailable\") Cmd.pageText (observedSlowStderr observation)))"
  alerted <- awaitOutput owner
    "slowState <- R.call (slowView (R.client slowWatcher)) ()\ninspectFull slowState"
    (Text.isInfixOf "slowAlert = Just")
  check "record actor observes the parent's shared job and retains one alert"
    ("slowChecked = True" `Text.isInfixOf` alerted
      && "slowAlert = Just" `Text.isInfixOf` alerted)
  void $ turn owner "Cmd.sendInput slowJob \"go\\n\"\nCmd.closeInput slowJob\nCmd.await slowJob"
  watched <- awaitOutput owner
    "slowState <- R.call (slowView (R.client slowWatcher)) ()\ninspectFull slowState"
    (Text.isInfixOf "slowCompletion = Just")
  check "the same watcher records later completion without another alert"
    ("slowAlert = Just" `Text.isInfixOf` watched
      && "slowCompletion = Just" `Text.isInfixOf` watched)
