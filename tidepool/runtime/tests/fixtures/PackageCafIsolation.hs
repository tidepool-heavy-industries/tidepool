module PackageCafIsolation
  ( packageText
  , packageProbe
  , packageSibling
  , packageLater
  ) where

import qualified Data.IntMap.Strict as IntMap

-- Character escaping reaches the real package implementation and its lazy
-- tables. The test must identify a selected package CAF from issued products.
{-# NOINLINE packageText #-}
packageText :: String
packageText = show (['\NUL' .. '\US'] ++ ['\DEL'])

packageProbe :: Int -> Int
packageProbe salt = length packageText + salt

packageSibling :: Char -> Int
packageSibling character = length (show character)

packageLater :: Int -> Int
packageLater key = IntMap.findWithDefault 0 key (IntMap.fromList [(11, 19), (37, 23)])
