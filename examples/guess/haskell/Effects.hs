{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts #-}
module Effects (module Effects, module Control.Monad.Freer) where

import Control.Monad.Freer

-- Console: emit a line, print a prompt, await an integer from stdin
data Console a where
  Emit     :: String -> Console ()
  Prompt   :: String -> Console ()
  AwaitInt :: Console Int

emit :: Member Console effs => String -> Eff effs ()
emit = send . Emit

prompt :: Member Console effs => String -> Eff effs ()
prompt = send . Prompt

awaitInt :: Member Console effs => Eff effs Int
awaitInt = send AwaitInt

-- Rng: generate random int in [lo, hi]
data Rng a where
  RandInt :: Int -> Int -> Rng Int

randInt :: Member Rng effs => Int -> Int -> Eff effs Int
randInt lo hi = send (RandInt lo hi)
