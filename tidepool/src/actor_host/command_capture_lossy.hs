import qualified Tidepool.Inspection as Inspection
degraded <- Cmd.readCommand job
let lossy = case degraded of { Right value -> value; Left issue -> error ("readCommand refused a finished job: " <> T.pack (show issue)) }
case Cmd.capturedStdout lossy of { Cmd.CapturePartial page _ -> if Cmd.outputLossy page && Cmd.outputLostBytes page > 0 && not (Cmd.outputFinished page) && Cmd.outputEnd page < Cmd.outputAvailableEnd page then ("loss-signals-survive" :: Text) else error "the page reached the caller with its loss signals flattened"; other -> error ("a lossy stream was reported as complete: " <> T.pack (show other)) }
case Cmd.capturedStderr lossy of { Cmd.CapturePartial _ _ -> ("both-streams-partial" :: Text); other -> error ("stderr loss was collapsed into stdout's verdict: " <> T.pack (show other)) }
case (Cmd.commandOutcome (Cmd.capturedResult lossy), Cmd.commandCleanup (Cmd.capturedResult lossy)) of { (Cmd.CommandExited 3, Cmd.CommandClean) -> ("outcome-survives-loss" :: Text); other -> error ("a failed capture rewrote the outcome: " <> T.pack (show other)) }
stderrOnly <- Cmd.readStderr job
case stderrOnly of { Cmd.CapturePartial _ _ -> ("readStderr-partial" :: Text); other -> error ("readStderr claimed a complete stream: " <> T.pack (show other)) }
let shown = fst (Inspection.displayWith 4096 lossy)
if T.isInfixOf "INCOMPLETE capture" shown && T.isInfixOf "display excerpt" shown && T.isInfixOf "lossy UTF-8" shown && not (T.isInfixOf "stdout · complete capture" shown) then ("incomplete-cannot-read-as-complete" :: Text) else error ("an incomplete capture displayed as complete: " <> shown)
