-- P3 oracle fixture 1: two targets import the same package symbol while
-- reaching different subsets of one defining module.
--
-- `onlyPack` reaches Data.Text.Internal.pack alone; `packThenUnpack` reaches
-- pack and unpack. The current compiler emits package products per binding,
-- so both targets resolve the shared `pack` worker to the same immutable
-- envelope. Reversing target order must not change either target's exact
-- ordered slots, body, or child history. The same-seal/different-body case is
-- covered by the authenticated historical artifacts under `historical/`.
module SameSealTwoProducts where

import Data.Text (Text, pack, unpack)

{-# NOINLINE onlyPack #-}
onlyPack :: String -> Text
onlyPack = pack

{-# NOINLINE packThenUnpack #-}
packThenUnpack :: String -> String
packThenUnpack = unpack . pack
