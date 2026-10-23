{-# OPTIONS_GHC -fwrite-interface -O1 -fno-write-if-simplified-core -fno-omit-interface-pragmas -fexpose-all-unfoldings #-}
-- Public INLINE metadata is available without any defining Core.
module ThinFixture where

{-# INLINE thinIdentity #-}
thinIdentity :: Int -> Int
thinIdentity value = value
