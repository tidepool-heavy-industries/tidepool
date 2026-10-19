import qualified Project.Work as Work
import qualified Tidepool.Effects.Authored as Files
import qualified Tidepool.Effects.Core as Effects
import qualified Data.Text as Text

bOnly <- pure (Work.readStamp (Work.Stamp 20 22))
Right workBExpected <- Files.readFile "reload-inputs/work-b.txt"
Right (runningBRevision, runningB) <- readSourceModule "Project.Work"
Right coherentSourceStatus <- Files.sourceStatus
display (Text.unlines
  [ "coherent-use-b constituents"
  , "bOnly=" <> Text.pack (show bOnly)
  , "answerFn1=" <> Text.pack (show (Work.answerFn 1))
  , "sourceBodiesEqual=" <> Text.pack (show (runningB == workBExpected))
  , "readSourceRevision=" <> runningBRevision
  , "activeRevision=" <> Effects.revisionIdentity (Effects.statusActive coherentSourceStatus)
  , "diskRevision=" <> Effects.revisionIdentity (Effects.statusDisk coherentSourceStatus)
  , "activeWorkDigests=" <> Text.pack (show [(Effects.sourceModuleName source, Effects.sourceModuleDigest source) | source <- Effects.revisionModules (Effects.statusActive coherentSourceStatus), Effects.sourceModuleName source == "Project.Work"])
  , "diskWorkDigests=" <> Text.pack (show [(Effects.sourceModuleName source, Effects.sourceModuleDigest source) | source <- Effects.revisionModules (Effects.statusDisk coherentSourceStatus), Effects.sourceModuleName source == "Project.Work"])
  ])
display (bOnly == 42 && Work.answerFn 1 == 42 && runningB == workBExpected
  )
