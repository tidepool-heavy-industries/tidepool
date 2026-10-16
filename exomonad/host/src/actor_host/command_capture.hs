import qualified Tidepool.Inspection as Inspection
captured <- Cmd.readCommand job
let capture = case captured of { Right value -> value; Left issue -> error ("readCommand refused a finished job: " <> T.pack (show issue)) }
let outcome = Cmd.capturedResult capture
let outcomePreserved = case (Cmd.commandOutcome outcome, Cmd.commandCleanup outcome) of { (Cmd.CommandExited 3, Cmd.CommandClean) -> True; other -> error ("outcome was rewritten: " <> T.pack (show other)) }
let streamsCaptured = case (Cmd.capturedStdout capture, Cmd.capturedStderr capture) of { (Cmd.CaptureComplete "standard out", Cmd.CaptureComplete "boom: file not found") -> True; other -> error ("streams not captured whole: " <> T.pack (show other)) }
stderrOnly <- Cmd.readStderr job
let stderrCaptured = case stderrOnly of { Cmd.CaptureComplete "boom: file not found" -> True; other -> error ("readStderr did not return complete stderr: " <> T.pack (show other)) }
let shown = fst (Inspection.displayWith 4096 capture)
let renderedStreams = T.isInfixOf "standard out" shown && T.isInfixOf "boom: file not found" shown
let stdoutGated = case Cmd.stdout finished of { Left (Cmd.Unsuccessful (Cmd.CommandExited 3)) -> True; other -> error ("Cmd.stdout changed: " <> T.pack (show other)) }
priorReader <- Cmd.readStdout job
let readStdoutGated = case priorReader of { Left (Cmd.Unsuccessful (Cmd.CommandExited 3)) -> True; other -> error ("Cmd.readStdout changed: " <> T.pack (show other)) }
display (outcomePreserved && streamsCaptured && stderrCaptured && renderedStreams && stdoutGated && readStdoutGated)
