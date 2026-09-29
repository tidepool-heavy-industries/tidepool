module Next (module Joined, c, nextValue) where
import Joined hiding (c, oldBool)
c :: Int
c = 99
nextValue :: Int
nextValue = case hiddenValue of Hidden value -> value
