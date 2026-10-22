module TransitiveHomeLeaf where

import qualified Data.Text as Text

{-# NOINLINE leafValue #-}
leafValue :: Int
leafValue = Text.length (Text.pack "transitive HOME package demand")
