import Tidepool.Aeson.Value (Value)
import qualified Tidepool.Inspection as Inspection
let retained = Cmd.job finished
let page text = Cmd.CommandPage { Cmd.outputText = text, Cmd.outputStart = 0, Cmd.outputEnd = T.length text, Cmd.outputAvailableEnd = T.length text, Cmd.outputRetainedStart = 0, Cmd.outputLostBytes = 0, Cmd.outputFinished = True, Cmd.outputLossy = False, Cmd.outputLeadingFragment = False, Cmd.outputTrailingFragment = False }
let outcome = Cmd.CommandResult { Cmd.commandOutcome = Cmd.CommandExited 0, Cmd.commandCleanup = Cmd.CommandClean }
let captured out err = Cmd.Finished retained outcome (Cmd.CommandOutput (page out) (page err))
let large = captured (T.replicate 32768 "λ") (T.replicate 32768 "z")
let rendering = Inspection.workbenchDisplay (inspectFull (Just [Right large :: Either Text Cmd.RunResult]))
let rendered = fst rendering
let bounded = Inspection.displayWith 1024 [large, large, large]
if T.length rendered <= 65536 && snd rendering && Cmd.stdout large == Right (T.replicate 32768 "λ") && T.isInfixOf (T.replicate 1024 "λ") rendered && T.isInfixOf (T.replicate 1024 "z") rendered && T.length (fst bounded) <= 1024 && snd bounded then pure ("large-display-ok" :: Text) else error "large display failed"
let goodJSON = Cmd.decodeWith (Cmd.asJSON @Value) (Cmd.stdout (captured "{\"ok\":true}" ""))
case goodJSON of { Right _ -> pure ("json-ok" :: Text); Left _ -> error "JSON decode failed" }
let partial = (page "true") { Cmd.outputStart = 100, Cmd.outputEnd = 104, Cmd.outputAvailableEnd = 104 }
let incomplete = Cmd.Finished retained outcome (Cmd.CommandOutput partial (page ""))
case Cmd.decodeWith (Cmd.asJSON @Value) (Cmd.stdout incomplete) of { Left (Cmd.OutputProblem _) -> pure ("partial-rejected" :: Text); _ -> error "partial JSON accepted" }
let stderrPartial = Cmd.Finished retained outcome (Cmd.CommandOutput (page "true") partial)
case Cmd.stdout stderrPartial of { Right "true" -> pure ("streams-independent" :: Text); _ -> error "stderr invalidated stdout" }
case Cmd.decodeWith (Cmd.asJSON @Value) (Cmd.stdout (captured "{" "")) of { Left (Cmd.DecodeProblem _) -> pure ("decode-error-distinct" :: Text); _ -> error "decode error lost" }
:info Cmd.RunResult
let lost = (page (T.replicate 12000 "x")) { Cmd.outputStart = 1000, Cmd.outputEnd = 13000, Cmd.outputAvailableEnd = 13000, Cmd.outputRetainedStart = 1000 }
let shortened = fst (Inspection.displayWith 1024 (Cmd.CommandOutput lost (page "")))
if T.isInfixOf "retention loss" shortened && T.isInfixOf "display tail" shortened && not (T.isInfixOf "earlier available" shortened) then pure ("omission-kinds-preserved" :: Text) else error "omission metadata lost"
