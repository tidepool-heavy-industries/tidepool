{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE ConstraintKinds #-}
module RecoveredEntryCaller where

import Control.Monad.Freer.Internal (Arrs, Eff, qApp)

data Dict constraint where
  Dict :: constraint => Dict constraint

{-# OPAQUE applicativeDictionary #-}
applicativeDictionary :: Dict (Applicative (Eff '[]))
applicativeDictionary = Dict

{-# OPAQUE caller #-}
caller :: Arrs '[] Int Int -> Int -> Eff '[] Int
caller = qApp

{-# OPAQUE flag #-}
flag :: Bool
flag = True

-- A CAF can return a function without acquiring that function's entry arity.
{-# OPAQUE functionResult #-}
functionResult :: Int -> Int
functionResult = case flag of
  True -> \value -> value
  False -> \value -> value + 1
