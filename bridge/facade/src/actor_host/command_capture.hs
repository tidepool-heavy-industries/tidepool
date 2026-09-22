import qualified Tidepool.Inspection as Inspection
captured <- Cmd.readCommand job
let capture = case captured of { Right value -> value; Left issue -> error ("readCommand refused a finished job: " <> T.pack (show issue)) }
let outcome = Cmd.capturedResult capture
case (Cmd.commandOutcome outcome, Cmd.commandCleanup outcome) of { (Cmd.CommandExited 3, Cmd.CommandClean) -> ("outcome-unreinterpreted" :: Text); other -> error ("outcome was rewritten: " <> T.pack (show other)) }
case (Cmd.capturedStdout capture, Cmd.capturedStderr capture) of { (Cmd.CaptureComplete "standard out", Cmd.CaptureComplete "boom: file not found") -> ("failed-streams-captured" :: Text); other -> error ("streams not captured whole: " <> T.pack (show other)) }
stderrOnly <- Cmd.readStderr job
case stderrOnly of { Cmd.CaptureComplete "boom: file not found" -> ("stderr-read" :: Text); other -> error ("readStderr did not return complete stderr: " <> T.pack (show other)) }
if T.isInfixOf "stdout · complete capture" (fst (Inspection.displayWith 4096 capture)) && T.isInfixOf "boom: file not found" (fst (Inspection.displayWith 4096 capture)) then ("capture-display-ok" :: Text) else error "capture display lost a stream"
case Cmd.stdout finished of { Left (Cmd.Unsuccessful (Cmd.CommandExited 3)) -> ("stdout-still-gated" :: Text); other -> error ("Cmd.stdout changed: " <> T.pack (show other)) }
priorReader <- Cmd.readStdout job
case priorReader of { Left (Cmd.Unsuccessful (Cmd.CommandExited 3)) -> ("readStdout-still-gated" :: Text); other -> error ("Cmd.readStdout changed: " <> T.pack (show other)) }
