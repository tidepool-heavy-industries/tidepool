answer :: Int -> Int
answer value = value + 1
{-# NOINLINE answer #-}

data HiddenResult = HiddenResult Int deriving Show

makeResult :: Int -> HiddenResult
makeResult value = HiddenResult (answer value)
{-# NOINLINE makeResult #-}
