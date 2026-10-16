import qualified Tidepool.Inspection as Inspection
degraded <- Cmd.readCommand job
let lossy = case degraded of { Right value -> value; Left issue -> error ("readCommand refused a finished job: " <> T.pack (show issue)) }
let lossSignalsPreserved = case Cmd.capturedStdout lossy of { Cmd.CapturePartial page _ -> if Cmd.outputLossy page && Cmd.outputLostBytes page > 0 && not (Cmd.outputFinished page) && Cmd.outputEnd page < Cmd.outputAvailableEnd page then True else error "the page reached the caller with its loss signals flattened"; other -> error ("a lossy stream was reported as complete: " <> T.pack (show other)) }
let bothStreamsPartial = case Cmd.capturedStderr lossy of { Cmd.CapturePartial _ _ -> True; other -> error ("stderr loss was collapsed into stdout's verdict: " <> T.pack (show other)) }
let outcomePreserved = case (Cmd.commandOutcome (Cmd.capturedResult lossy), Cmd.commandCleanup (Cmd.capturedResult lossy)) of { (Cmd.CommandExited 3, Cmd.CommandClean) -> True; other -> error ("a failed capture rewrote the outcome: " <> T.pack (show other)) }
stderrOnly <- Cmd.readStderr job
let stderrPartial = case stderrOnly of { Cmd.CapturePartial _ _ -> True; other -> error ("readStderr claimed a complete stream: " <> T.pack (show other)) }
let shown = fst (Inspection.displayWith 4096 lossy)
let renderedStreams = T.isInfixOf "standard out" shown && T.isInfixOf "boom: file not found" shown
display (lossSignalsPreserved && bothStreamsPartial && outcomePreserved && stderrPartial && renderedStreams)
