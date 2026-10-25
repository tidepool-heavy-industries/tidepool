module LazyBackedgeDependency (observeToken) where
import {-# SOURCE #-} LazyBackedgeTarget (Token)
import OptionalAnchor (anchor)

{-# NOINLINE observeToken #-}
observeToken :: Token -> Int
observeToken _ = anchor + 41
