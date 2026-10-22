module PriorSourceBodies (plainNumber, textNumber) where

import qualified Data.Text as Text

{-# NOINLINE plainNumber #-}
plainNumber :: Int -> Int
plainNumber value = value

{-# NOINLINE textNumber #-}
textNumber :: String -> Int
textNumber value = Text.length (Text.pack value)
