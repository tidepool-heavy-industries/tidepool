import qualified Project.Work as Work
import qualified Tidepool.Effects.Authored as Files
Right before <- Files.readFile "effect-count.txt"
Right () <- Files.writeFile "effect-count.txt" (before <> "x")
aOnly <- pure (7 :: Int)
display (Work.answerFn 1 == 2)
