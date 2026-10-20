import qualified Project.Work as Work
import qualified Tidepool.Effects.Authored as Files
import qualified Tidepool.Effects.Core as Effects

bOnly <- pure (Work.readStamp (Work.Stamp 20 22))
Right expectedB <- Files.readFile "reload-inputs/work-b.txt"
Right expectedC <- Files.readFile "reload-inputs/work-c.txt"
Right (_, runningB) <- readSourceModule "Project.Work"
Right onDiskC <- Files.readFile ".exomonad/Project/Work.hs"
Right sourceState <- Files.sourceStatus
display (bOnly == 42 && Work.answerFn 1 == 42
  && runningB == expectedB && onDiskC == expectedC && runningB /= onDiskC
  && Effects.revisionIdentity (Effects.statusActive sourceState)
    /= Effects.revisionIdentity (Effects.statusDisk sourceState))
