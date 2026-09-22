{-# LANGUAGE OverloadedStrings #-}

module ContainersContract where

import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Set (Set)
import Data.Set qualified as Set
import Data.Text (Text)

-- Every probe is a nullary monomorphic top observed as lists/tuples/ints.
-- Map keys are Text so probe answers depend on the real Ord Text instance.
inventory :: Map Text Int
inventory = Map.fromList [("pear", 3), ("apple", 1), ("quince", 7), ("banana", 2)]

-- Depends on Ord Text: toList yields ascending key order.
mapToAscList :: [(Text, Int)]
mapToAscList = Map.toList inventory

insertWithTotal :: Int
insertWithTotal =
  Map.findWithDefault 0 "apple" (Map.insertWith (+) "apple" 10 inventory)

adjustedLookup :: Maybe Int
adjustedLookup = Map.lookup "pear" (Map.adjust (* 2) "pear" inventory)

-- Observes both the surviving size and the updated value at "quince", so a
-- broken alter update branch (as opposed to its delete branch) fails this
-- probe.
alteredSize :: (Int, Maybe Int)
alteredSize =
  ( Map.size altered
  , Map.lookup "quince" altered
  )
  where
    altered =
      Map.alter (const Nothing) "banana"
        (Map.alter (fmap (+ 1)) "quince" inventory)

foldedKeys :: Text
foldedKeys = Map.foldrWithKey (\key _ folded -> key <> "," <> folded) "" inventory

unionTotals :: [(Text, Int)]
unionTotals =
  Map.toList
    (Map.unionWith (+) inventory (Map.fromList [("apple", 5), ("cherry", 4)]))

names :: Set Text
names = Set.fromList ["apple", "pear", "quince"]

setMembership :: (Bool, Bool)
setMembership = (Set.member "apple" names, Set.member "durian" names)

-- Union then difference; Set.toList order also depends on Ord Text.
setDifferenceList :: [Text]
setDifferenceList =
  Set.toList
    (Set.difference
      (Set.union names (Set.fromList ["banana", "cherry"]))
      (Set.fromList ["pear", "cherry"]))
