{-# LANGUAGE DataKinds, ExplicitForAll, KindSignatures, MagicHash #-}
{-# LANGUAGE ScopedTypeVariables, TypeApplications #-}
module RepPoly where

import GHC.Exts (RuntimeRep, TYPE, Int#, (+#))
import GHC.Types (Int(I#))

{-# NOINLINE applyTo #-}
applyTo :: forall (r :: RuntimeRep) a (b :: TYPE r). (a -> b) -> a -> b
applyTo function value = function value

{-# NOINLINE atInt #-}
atInt :: Int -> Int
atInt value = applyTo (\n -> n) value

{-# NOINLINE atPrim #-}
atPrim :: Int -> Int#
atPrim value = applyTo (\(I# n) -> n +# 1#) value

{-# NOINLINE applyJoin #-}
applyJoin :: forall (r :: RuntimeRep) (b :: TYPE r). (Int -> b) -> Int -> b
applyJoin function value =
  let {-# NOINLINE go #-}
      go :: Int -> b
      go argument = function argument
  in if value < 0 then go (negate value) else go value

result :: Int
result = case atInt 20 of I# left -> I# (left +# atPrim 21)

joined :: Int
joined = applyJoin (\(I# n) -> I# (n +# 1#)) 41
