module Main where
import Next
main :: IO ()
main = print (c, nextValue, oldInt, oldAssociated, present (0 :: Double))
present :: C a => a -> Bool
present _ = True
