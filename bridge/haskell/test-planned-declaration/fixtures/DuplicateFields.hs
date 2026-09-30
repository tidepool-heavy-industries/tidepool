{-# LANGUAGE DuplicateRecordFields #-}
module DuplicateFields (One(..), Two(..)) where

data One = One { same :: Int }
data Two = Two { same :: Bool }
