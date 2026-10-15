import qualified Tidepool.Effects.Authored as Files
Right invalidWork <- Files.readFile "reload-inputs/work-invalid.txt"
Right () <- Files.writeFile ".exomonad/Project/Work.hs" invalidWork
display (aClosure 1 == 2 && Work.readStamp aToken == 7)
