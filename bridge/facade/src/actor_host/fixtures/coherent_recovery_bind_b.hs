import qualified Project.Work as Work
import qualified Project.ExtraCheck as Extra
bOnly <- pure (Work.readStamp (Work.Stamp 20 22))
inspectFull (bOnly == 42 && Work.answerFn 1 == 42 && Extra.extraCheck == 73)
