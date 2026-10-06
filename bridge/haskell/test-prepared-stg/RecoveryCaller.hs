{-# LANGUAGE GADTs #-}

module RecoveryCaller where

import Prelude (Int, fst, (+), (*), (.))
import Data.Functor.Identity (Identity(..))
import Data.FTCQueue (FTCQueue, ViewL(..), tsingleton, tviewl, (><))
import RecoveryHome (homeValue)

caller :: Int
caller = homeValue (fst (1, ()))

-- Two independent queue operations share Leaf/Node's implicit worker owner.
queueProbe :: Int
queueProbe = applyQueue (tsingleton (Identity . (+ 1)) >< tsingleton (Identity . (* 2))) 20

applyQueue :: FTCQueue Identity a b -> a -> b
applyQueue queue input = case tviewl queue of
  TOne operation -> runIdentity (operation input)
  operation :| rest -> applyQueue rest (runIdentity (operation input))
