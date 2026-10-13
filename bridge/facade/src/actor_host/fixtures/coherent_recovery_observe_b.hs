import qualified Tidepool.Effects.Authored as Files
import qualified Persona.Memory as Memory
Right (_, workB) <- readSourceModule "Project.Work"
Right (_, extraB) <- readSourceModule "Project.ExtraCheck"
Right expectedWorkB <- Files.readFile "reload-inputs/work-b.txt"
Right expectedExtraB <- Files.readFile "reload-inputs/extra-b.txt"
Right marker <- Files.readFile "initializer-count.txt"
Right memory <- Memory.recall Memory.Human
display (workB == expectedWorkB && extraB == expectedExtraB && Work.answerFn 1 == 42 && bOnly == 42 && Extra.extraCheck == 73 && marker == "x" && memory == "memory-C")
