import qualified Data.Text as T
import qualified Tidepool.Command as Cmd

let offlineRustJob = __OFFLINE_RETAINED_JOB__
offlineRustTerminal <- Cmd.await offlineRustJob
let offlineTestsRan = case Cmd.stdout offlineRustTerminal of
      Right output -> all (`T.isInfixOf` output)
        [ "test result: ok. 3 passed"
        , "empty_input_has_zero_score ... ok"
        , "mixed_signs_preserve_their_contribution ... ok"
        , "an_all_negative_score_stays_negative ... ok"
        ]
      Left _ -> False
display (offlineTestsRan
  && Cmd.commandOutcome (Cmd.commandResult offlineRustTerminal) == Cmd.CommandExited 0
  && Cmd.commandCleanup (Cmd.commandResult offlineRustTerminal) == Cmd.CommandClean)
