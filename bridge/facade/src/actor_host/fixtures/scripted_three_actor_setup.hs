let retainedValue = 41 :: Int
let retainedAction = (pure retainedValue :: Eff effects Int)
value <- retainedAction
display (value == 41)
