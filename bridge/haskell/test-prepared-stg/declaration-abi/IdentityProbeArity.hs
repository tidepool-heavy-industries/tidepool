{-# LANGUAGE DataKinds #-}
module Probe (Public, public, Classy(..), Newtype(..), runMutual, unrelated) where

import Data.Text qualified as Direct
import IdentityReexport (Text)
import Control.Monad.Freer (Eff)

data Signal a = Signal
data ImportedControls = ImportedControls Text Direct.Text (Eff '[Signal] Text)


data TrulyUnused = TrulyUnused0 Int | TrulyUnused1 Bool

data Public = Pub0 Int | Pub1 Int

data Private = Priv0 Int | Priv1 Int Int Int

private :: Int -> Private
private n = if n == 0 then Priv0 n else Priv1 n n n

public :: Int -> Int
public n = case private n of
  Priv0 x -> x
  Priv1 x y z -> x + y + z

class Classy a where
  classy :: a -> Int

instance Classy Int where
  classy = id

newtype Newtype = Newtype Int

data LeftR = LeftR RightR
data RightR = RightR LeftR

runMutual :: LeftR -> Int
runMutual (LeftR (RightR _)) = 1

unrelated :: Int -> Int
unrelated n = n + 1
