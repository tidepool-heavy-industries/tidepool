{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts #-}
module Guess where

import Control.Monad.Freer

-- Console: emit a line, await an integer from stdin
data Console a where
  Emit     :: String -> Console ()
  AwaitInt :: Console Int

emit :: Member Console effs => String -> Eff effs ()
emit = send . Emit

awaitInt :: Member Console effs => Eff effs Int
awaitInt = send AwaitInt

-- Rng: generate random int in [lo, hi]
data Rng a where
  RandInt :: Int -> Int -> Rng Int

randInt :: Member Rng effs => Int -> Int -> Eff effs Int
randInt lo hi = send (RandInt lo hi)

-- Game logic
game :: Eff '[Console, Rng] ()
game = do
  target <- randInt 1 100
  emit "I'm thinking of a number between 1 and 100."
  guessLoop target

guessLoop :: Int -> Eff '[Console, Rng] ()
guessLoop target = do
  emit "Your guess? "
  guess <- awaitInt
  if guess == target
    then emit "Correct!"
    else do
      emit (if guess < target then "Too low!" else "Too high!")
      guessLoop target
