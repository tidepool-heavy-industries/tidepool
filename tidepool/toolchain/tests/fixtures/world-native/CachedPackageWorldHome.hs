module CachedPackageWorldHome where

import qualified Data.Text as Text

{-# NOINLINE cachedTextLength #-}
cachedTextLength :: String -> Int
cachedTextLength = Text.length . Text.pack

{-# NOINLINE cachedIdentity #-}
cachedIdentity :: Int -> Int
cachedIdentity value = value
