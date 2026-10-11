answer <- recursiveHelper
let nominalAnswer = case (sessionInput :: (RecursiveRootInput, RecursiveChildInput)) of (RecursiveRootInput rootPart, RecursiveChildInput childPart) -> rootPart + childPart
display (nominalAnswer == 42 && answer == 42)
respond (if answer == 42 then nominalAnswer else 0)
