module MemoReuseDependency where

import OptionalAnchor (anchor)

{-# NOINLINE answer #-}
answer :: Int -> Int
answer value = value + anchor
