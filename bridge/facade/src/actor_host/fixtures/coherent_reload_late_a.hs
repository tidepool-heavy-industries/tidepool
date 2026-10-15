import qualified Tidepool.Effects.Authored as Files

Right workAExpected <- Files.readFile "reload-inputs/work-a.txt"
Right (_, runningA) <- readSourceModule "Project.Work"
aLate <- pure (Work.readStamp aToken)
display (aOnly == 7 && aLate == 7
  && aClosure 1 == 2 && runningA == workAExpected)
