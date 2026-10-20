module Project.Work (Stamp(..), readStamp, answerFn) where

data Stamp = Stamp Int Int Int
readStamp :: Stamp -> Int
readStamp (Stamp x y z) = x + y + z
answerFn :: Int -> Int
answerFn x = x + 99
