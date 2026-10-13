import qualified Project.Work as Work
import qualified Project.ExtraCheck as Extra
-- Authored declarations survive cold recovery; statement bindings belong to this heap.
bRemembered :: Int
bRemembered = Work.readStamp (Work.Stamp 20 22)
bOnly <- pure bRemembered
display (bOnly == 42 && bRemembered == 42 && Work.answerFn 1 == 42 && Extra.extraCheck == 73)
