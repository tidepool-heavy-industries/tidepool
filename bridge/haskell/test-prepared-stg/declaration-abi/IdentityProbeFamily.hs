{-# LANGUAGE DataKinds #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE TypeFamilies #-}
module Probe (Public, public, Classy(..), Newtype(..), runMutual, unrelated, promoted) where

import Data.Text qualified as Direct
import IdentityReexport (Text)
import Control.Monad.Freer (Eff)

data Signal a = Signal
data ImportedControls = ImportedControls Text Direct.Text (Eff '[Signal] Text)


data Public = Pub0 Int | Pub1 Int

data Private = Priv0 Int | Priv1 Int Int

data TrulyUnused = TrulyUnused0 Int | TrulyUnused1 Bool

private :: Int -> Private
private n = if n == 0 then Priv0 n else Priv1 n n

public :: Int -> Int
public n = case private n of
  Priv0 x -> x
  Priv1 x y -> x + y

class Classy a where
  classy :: a -> Int

instance Classy Int where
  classy = id

newtype Newtype = Newtype Int

data LeftR = LeftR RightR
data RightR = RightR LeftR

runMutual :: LeftR -> Int
runMutual (LeftR (RightR _)) = 1

data family Family a
data instance Family Int = FamilyRep Int

familyUse :: Int -> Family Int
familyUse = FamilyRep

data Promotion = PromoA | PromoB
data Proxy (a :: Promotion) = Proxy

promoted :: Proxy 'PromoA -> Int
promoted Proxy = 1

unrelated :: Int -> Int
unrelated n = n + 1
