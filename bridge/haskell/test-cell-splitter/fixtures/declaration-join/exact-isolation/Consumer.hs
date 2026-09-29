{-# LANGUAGE TypeFamilies #-}
module Main where
import Joined
main :: IO ()
main = print (oldInt, oldBool, c (0 :: Int), c (0 :: Double), oldFD,
  d (0 :: Int) :: Char, oldFamily, (3 :: F Char), ('z' :: Associated Char), forced, c [0 :: Int])
  where forced = case hiddenValue of Hidden x -> x
