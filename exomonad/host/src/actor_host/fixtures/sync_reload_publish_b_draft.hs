import qualified Tidepool.Effects.Authored as Files
Right workB <- Files.readFile "reload-inputs/work-b.txt"
Right promptB <- Files.readFile "reload-inputs/prompt-b.txt"
Right specB <- Files.readFile "reload-inputs/spec-b.txt"
Right () <- Files.writeFile ".exomonad/Project/Work.hs" workB
Right () <- Files.writeFile ".exomonad/prompts/agent.md" promptB
Right () <- Files.writeFile ".exomonad/AgentSpec.hs" specB
display (aClosure 1 == 2 && Work.readStamp aToken == 7)
