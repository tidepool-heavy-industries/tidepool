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
    , "batch <- runProbeBatch (ProbeLimits 2 2) commandProbes"
    ]
  assertCell owner "both supplied probes finish with independent exact job handles"
    "case batch of { Right completed -> case observedProbes completed of { [ProbeObserved \"first\" firstJob firstReceipt (Right firstOut) _, ProbeObserved \"second\" secondJob secondReceipt (Right secondOut) _] -> firstJob /= secondJob && Cmd.commandOutcome firstReceipt == Cmd.CommandExited 0 && Cmd.commandOutcome secondReceipt == Cmd.CommandExited 0 && Cmd.pageText firstOut == \"first\" && Cmd.pageText secondOut == \"second\"; _ -> False }; _ -> False }"
  void $ turn owner "slowJob <- Cmd.background (Cmd.withStdin (Cmd.argv [\"sh\",\"-c\",\"read line; printf done\"]))\nRight slowWatcher <- watchSlowCommand me \"owned slow probe\" slowJob 10 64 (\\observation -> pure (either (const \"stdout unavailable\") Cmd.pageText (observedSlowStdout observation) <> either (const \"stderr unavailable\") Cmd.pageText (observedSlowStderr observation)))"
  awaitCell owner "ongoing slow-job subscription retains one typed alert"
    "do { state <- R.call (slowView (R.client slowWatcher)) (); pure (slowChecked state && case slowAlert state of { Just _ -> True; Nothing -> False }) }"
  void $ turn owner "Cmd.sendInput slowJob \"go\\n\"\nCmd.closeInput slowJob\nCmd.await slowJob"
  awaitCell owner "the same subscription records terminal completion and retains its alert"
    "do { state <- R.call (slowView (R.client slowWatcher)) (); pure (case (slowCompletion state, slowAlert state) of { (Just receipt, Just _) -> Cmd.commandOutcome receipt == Cmd.CommandExited 0; _ -> False }) }"
