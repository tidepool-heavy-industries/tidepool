import qualified Project.Work as Work
import qualified Tidepool.Lookup as L
import qualified Tidepool.Effects.Authored as Files

bOnly <- pure (Work.readStamp (Work.Stamp 20 22))
missing <- L.lookupRaw (L.lookupRequest ["aOnly", "aLate"])
Right workBExpected <- Files.readFile "reload-inputs/work-b.txt"
Right (_, runningB) <- readSourceModule "Project.Work"
display (bOnly == 42 && Work.answerFn 1 == 42 && runningB == workBExpected
  && all (\r -> case L.lookupOutcome r of { L.LookupMissing _ _ -> True; _ -> False }) (L.lookupResults missing) && length (L.lookupResults missing) == 2)
