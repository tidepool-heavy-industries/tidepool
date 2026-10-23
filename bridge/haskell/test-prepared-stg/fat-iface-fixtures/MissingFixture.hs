{-# OPTIONS_GHC -fwrite-interface -O1 -fwrite-if-simplified-core -fno-omit-interface-pragmas -fexpose-all-unfoldings #-}
-- The retained owner supplies Core independently of this output file's lifetime.
module MissingFixture where

{-# INLINE missingIdentity #-}
missingIdentity :: Int -> Int
missingIdentity value = value
