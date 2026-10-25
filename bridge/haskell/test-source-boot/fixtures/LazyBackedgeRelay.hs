module LazyBackedgeRelay (relayToken) where
import LazyBackedgeDependency (observeToken)

{-# NOINLINE relayToken #-}
relayToken = observeToken
