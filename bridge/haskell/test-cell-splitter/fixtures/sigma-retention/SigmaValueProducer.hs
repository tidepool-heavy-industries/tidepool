{-# LANGUAGE GHC2024, NoMonomorphismRestriction, RankNTypes #-}
module SigmaValueProducer where

-- One monomorphic action returns a value containing a genuine dictionary
-- closure. The forall belongs to the value, not to the action performing it.
newtype SigmaNumber = SigmaNumber { sigmaNumber :: forall a. Num a => a }
__result :: IO SigmaNumber
__result = pure (SigmaNumber scalar)

scalar :: forall a. Num a => a
scalar = 1

plain :: Int
plain = 1

function :: Int -> Int
function = (+ 1)

boxedEquality :: (a ~ Int) => a
boxedEquality = 1

erased :: forall a. a
erased = undefined

data RankNum = RankNum (forall a. Num a => a)
nested :: RankNum
nested = RankNum scalar

data RankErased = RankErased (forall a. a)
nestedErased :: RankErased
nestedErased = RankErased undefined

data RecursiveNum = RecursiveNum RecursiveNum (forall a. Num a => a)
recursive :: RecursiveNum
recursive = undefined
