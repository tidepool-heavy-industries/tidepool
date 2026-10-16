module Project.Work (Stamp(..), readStamp, answerFn) where

data Stamp = Stamp Int
readStamp :: Stamp -> Int
readStamp (Stamp x) = x
answerFn :: Int -> Int
answerFn x = x + 1
