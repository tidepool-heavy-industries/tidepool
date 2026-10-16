import qualified Tidepool.Effects.Authored as Files
import qualified Persona.Memory as Memory
import qualified Tidepool.Lookup as L
Right (_, workB) <- readSourceModule "Project.Work"
Right (_, extraB) <- readSourceModule "Project.ExtraCheck"
Right expectedWorkB <- Files.readFile "reload-inputs/work-b.txt"
Right expectedExtraB <- Files.readFile "reload-inputs/extra-b.txt"
Right marker <- Files.readFile "initializer-count.txt"
Right memory <- Memory.recall Memory.Human
-- Live reload retains issued heap bindings. Restart restores declarations without replaying statements.
heapBinding <- L.lookupRaw (L.lookupRequest ["bOnly"])
display (workB == expectedWorkB && extraB == expectedExtraB && Work.answerFn 1 == 42 && bRemembered == 42 && Extra.extraCheck == 73 && marker == "x" && memory == "memory-C"
  && case L.lookupResults heapBinding of
       [result] -> case L.lookupOutcome result of { L.LookupMissing _ _ -> True; _ -> False }
       _ -> False)
