import qualified Tidepool.Effects.Authored as Files
import qualified Data.Text as DiagnosticText
Right workB <- Files.readFile "reload-inputs/work-b.txt"
Right promptB <- Files.readFile "reload-inputs/prompt-b.txt"
Right extraB <- Files.readFile "reload-inputs/extra-b.txt"
Right () <- Files.writeFile ".exomonad/Project/Work.hs" workB
Right () <- Files.writeFile ".exomonad/prompts/agent.md" promptB
Right () <- Files.writeFile ".exomonad/Project/ExtraCheck.hs" extraB
publishedB <- reloadSource ["Project.ExtraCheck"] >>= either (error . DiagnosticText.pack . show) pure
inspectFull (case publishedB of { ReloadPublished _ _ _ -> True; _ -> False })
