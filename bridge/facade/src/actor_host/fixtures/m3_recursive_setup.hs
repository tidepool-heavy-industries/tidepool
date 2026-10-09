let recursiveSeed = 41 :: Int
let recursiveHelper = (pure (recursiveSeed + 1) :: Eff effects Int)
value <- recursiveHelper
display (value == 42)
