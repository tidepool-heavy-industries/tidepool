import qualified Project.Work as Work

aOnly <- pure (7 :: Int)
aToken <- pure (Work.Stamp aOnly)
aClosure <- pure Work.answerFn
display (Work.readStamp aToken == 7 && aClosure 1 == 2)
