{-# LANGUAGE TypeFamilies #-}
module Main where
import Joined
main :: IO ()
main = print (oldInt, oldBool, c (0 :: Int), c (0 :: Double), oldFD,
  d (0 :: Int) :: Char, oldFamily, (3 :: F Char), ('z' :: Associated Char), forced, c [0 :: Int], oldAssociated, ('j' :: J Char))
  where forced = case hiddenValue of Hidden x -> x
