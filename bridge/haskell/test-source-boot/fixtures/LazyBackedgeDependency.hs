module LazyBackedgeDependency (observeToken) where
import {-# SOURCE #-} LazyBackedgeTarget (Token)

{-# NOINLINE observeToken #-}
observeToken :: Token -> Int
observeToken _ = 42
