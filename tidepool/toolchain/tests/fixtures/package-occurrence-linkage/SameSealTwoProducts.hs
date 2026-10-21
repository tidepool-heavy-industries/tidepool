-- P3 oracle fixture 1: two targets import the same package symbol while
-- reaching different subsets of one defining module.
--
-- `onlyPack` reaches Data.Text.Internal.pack alone; `packThenUnpack` reaches
-- pack and unpack. Both targets import the same package symbol (pack) under
-- one defining-interface seal, but the retained bodies of the shared worker
-- differ (constructor/type renumbering). A (seal, symbol) selector cannot
-- tell the two bodies apart; an exact occurrence link can.
module SameSealTwoProducts where

import Data.Text (Text, pack, unpack)

{-# NOINLINE onlyPack #-}
onlyPack :: String -> Text
onlyPack = pack

{-# NOINLINE packThenUnpack #-}
packThenUnpack :: String -> String
packThenUnpack = unpack . pack
