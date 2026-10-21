module CanonicalPartial (partial) where

{-# NOINLINE partial #-}
partial :: Maybe Int -> Int
partial (Just value) = value
