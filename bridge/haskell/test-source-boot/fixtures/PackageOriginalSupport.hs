{-# LANGUAGE TypeApplications #-}
module PackageOriginalSupport where

import Data.List (reverse)
import Type.Reflection (TypeRep, typeRep)
import System.IO (Handle, stdout)
import PackageOriginalHome (homeValue)
import Tidepool.Session.Val.G7 (liveValue)

{-# NOINLINE packageFunction #-}
packageFunction :: [Int] -> [Int]
packageFunction = reverse

{-# NOINLINE packageConstructor #-}
packageConstructor :: Maybe (Maybe Int)
packageConstructor = Just Nothing

{-# NOINLINE packageNullaryArgument #-}
packageNullaryArgument :: (Maybe Int -> Int) -> Int
packageNullaryArgument f = f Nothing

{-# NOINLINE packageCAF #-}
packageCAF :: TypeRep Int
packageCAF = typeRep @Int

{-# NOINLINE packageThunk #-}
packageThunk :: Handle
packageThunk = stdout

{-# NOINLINE homeDemand #-}
homeDemand :: Int
homeDemand = homeValue

{-# NOINLINE valueDemand #-}
valueDemand :: Int
valueDemand = liveValue
