module RetainedNominal (Token(..), Leaf(..)) where

import RetainedNominalLeaf (Leaf(..))

data Token = Token Leaf deriving (Eq, Show)
