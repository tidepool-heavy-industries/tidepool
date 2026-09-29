{-# LANGUAGE MultiParamTypeClasses, TypeFamilies #-}
module Old (oldInt, oldFD, oldFamily, hiddenValue) where
import Common
instance C Int where c _ = hidden
instance C Double where c _ = 33
instance D Int Bool where d _ = True
type instance F Int = Bool
hidden :: Int
hidden = 11
{-# NOINLINE hidden #-}
oldInt :: Int
oldInt = c (0 :: Int)
{-# NOINLINE oldInt #-}
oldFD :: Bool
oldFD = d (0 :: Int)
oldFamily :: Bool
oldFamily = pass True where pass :: F Int -> Bool; pass x = x
hiddenValue :: Hidden
hiddenValue = Hidden hidden
