module PackageCafIsolation
  ( packageText
  , packageProbe
  , packageSibling
  ) where

-- Character escaping reaches the real package implementation and its lazy
-- tables. The test must identify a selected package CAF from issued products.
{-# NOINLINE packageText #-}
packageText :: String
packageText = show (['\NUL' .. '\US'] ++ ['\DEL'])

packageProbe :: Int -> Int
packageProbe salt = length packageText + salt

packageSibling :: Char -> Int
packageSibling character = length (show character)
