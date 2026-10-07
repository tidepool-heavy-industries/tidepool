{-# LANGUAGE GHC2024, RankNTypes #-}
module ConstructorEvidenceProducer where

import Data.Typeable (Typeable)

data Some where
  Some :: Typeable a => a -> Some
some :: Some
some = Some (41 :: Int)

data NumEvidence where
  NumEvidence :: Num a => a -> NumEvidence
number :: NumEvidence
number = NumEvidence (41 :: Int)

class (a ~ Int) => EqualInt a
instance EqualInt Int

data BoxedEquality where
  BoxedEquality :: EqualInt a => a -> BoxedEquality
equality :: BoxedEquality
equality = BoxedEquality (41 :: Int)

data PrimitiveEquality a where
  PrimitiveEquality :: Int -> PrimitiveEquality Int
primitive :: PrimitiveEquality Int
primitive = PrimitiveEquality 41

plain :: Int
plain = 1
function :: Int -> Int
function = (+ 1)
numScalar :: forall a. Num a => a
numScalar = 1
forallOnly :: forall a. a
forallOnly = undefined
boxedScalar :: forall a. (a ~ Int) => a
boxedScalar = 1

-- This action is monomorphic; its result stores a real constructor dictionary.
__result :: IO Some
__result = pure some
