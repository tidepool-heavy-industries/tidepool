import qualified Project.Work as Work
import qualified Tidepool.Effects.Authored as Files
import qualified Tidepool.Effects.Core as Effects
import qualified Data.Text as Text

bOnly <- pure (Work.readStamp (Work.Stamp 20 22))
Right expectedB <- Files.readFile "reload-inputs/work-b.txt"
Right expectedC <- Files.readFile "reload-inputs/work-c.txt"
Right (_, runningB) <- readSourceModule "Project.Work"
Right onDiskC <- Files.readFile ".exomonad/Project/Work.hs"
Right sourceState <- Files.sourceStatus
display (Text.unlines
  [ "frozen-b constituents"
  , "bOnly=" <> Text.pack (show bOnly)
  , "answerFn1=" <> Text.pack (show (Work.answerFn 1))
  , "runningSourceIsB=" <> Text.pack (show (runningB == expectedB))
  , "diskSourceIsC=" <> Text.pack (show (onDiskC == expectedC))
  , "activeRevision=" <> Effects.revisionIdentity (Effects.statusActive sourceState)
  , "diskRevision=" <> Effects.revisionIdentity (Effects.statusDisk sourceState)
  ])
display (bOnly == 42 && Work.answerFn 1 == 42
  && runningB == expectedB && onDiskC == expectedC && runningB /= onDiskC
  && Effects.revisionIdentity (Effects.statusActive sourceState)
    /= Effects.revisionIdentity (Effects.statusDisk sourceState))
