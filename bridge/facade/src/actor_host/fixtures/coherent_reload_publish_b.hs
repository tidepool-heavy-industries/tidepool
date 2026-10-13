import qualified Tidepool.Effects.Authored as Files

Right workB <- Files.readFile "reload-inputs/work-b.txt"
Right promptB <- Files.readFile "reload-inputs/prompt-b.txt"
Right () <- Files.writeFile ".exomonad/Project/Work.hs" workB
Right () <- Files.writeFile ".exomonad/prompts/agent.md" promptB
Right reloadB <- reloadSource []
aAfterReload <- pure (aClosure 1)
display (case reloadB of
  ReloadPublished _ _ _ -> aAfterReload == 2 && Work.readStamp aToken == 7
  _ -> False)
