module Project.Work (Stamp(..), readStamp, answerFn) where

data Stamp = Stamp Int Int
readStamp :: Stamp -> Int
readStamp (Stamp x y) = x + y
answerFn :: Int -> Int
answerFn x = x + 41
