{-# LANGUAGE OverloadedStrings #-}

module UserTypesContract where

import Data.Text (Text)
import Data.Text qualified as T

-- These seeds must reach the engine as real computation: each hides a
-- probe's operand from the simplifier so it cannot fold the probe's class
-- dispatch, record update, Functor/Foldable traversal, or Maybe/Either
-- chain into a pre-built literal or CAF at compile time.
--
-- A bare literal CAF (e.g. `{-# NOINLINE seed #-}; seed = 5`) is not
-- enough: GHC treats a manifest literal RHS as trivial and lets
-- case-of-known-constructor see through it for arithmetic constant
-- folding regardless of the NOINLINE/OPAQUE pragma on the CAF itself. A
-- NOINLINE function of at least one argument does not get this exemption,
-- so every scalar seed here is produced by applying one to a literal;
-- the pragma then genuinely blocks the simplifier from ever seeing that
-- the result is the literal it was built from.
{-# NOINLINE opaqueInt #-}
opaqueInt :: Int -> Int
opaqueInt x = x

orderQuantitySeed :: Int
orderQuantitySeed = opaqueInt 5

orderUnitPriceSeed :: Int
orderUnitPriceSeed = opaqueInt 250

pairLeftSeed :: Int
pairLeftSeed = opaqueInt 3

pairRightSeed :: Int
pairRightSeed = opaqueInt 4

singleSeed :: Int
singleSeed = opaqueInt 7

maybeLeftSeed :: Int
maybeLeftSeed = opaqueInt 6

maybeRightSeed :: Int
maybeRightSeed = opaqueInt 7

-- A NOINLINE list CAF does not need the function-wrapped treatment above:
-- traversing it requires inlining the recursive traverse loop itself,
-- which the simplifier's size budget does not do for a blocked unfolding.
{-# NOINLINE traverseSeeds #-}
traverseSeeds :: [Int]
traverseSeeds = [2, 4, 6, 8]

{-# NOINLINE traverseFailureSeeds #-}
traverseFailureSeeds :: [Int]
traverseFailureSeeds = [2, 5, 6]

-- A class with a default method and two instances measures dictionary
-- passing and default-method dispatch.
class Describable a where
  describe :: a -> Text
  summary :: a -> Text
  summary value = "summary:" <> describe value

data Widget = Widget

instance Describable Widget where
  describe Widget = "widget"

data Gadget = Gadget

instance Describable Gadget where
  describe Gadget = "gadget"
  summary Gadget = "custom-gadget"

-- NOINLINE keeps the class-method call unexpanded at the use site, so
-- dictionary selection and default-method dispatch happen as a real call
-- rather than being resolved away to the two instances' known results.
{-# NOINLINE summaryOf #-}
summaryOf :: Describable a => a -> Text
summaryOf x = summary x

defaultMethodPair :: (Text, Text)
defaultMethodPair = (summaryOf Widget, summaryOf Gadget)

-- A record with 5+ fields updated via record syntax.
data Order = Order
  { orderId :: Int
  , customer :: Text
  , quantity :: Int
  , unitPrice :: Int
  , expedited :: Bool
  , note :: Text
  }

baseOrder :: Order
baseOrder =
  Order
    { orderId = 41
    , customer = "ada"
    , quantity = 3
    , unitPrice = orderUnitPriceSeed
    , expedited = False
    , note = "none"
    }

updatedOrderView :: (Int, Text, Int, Bool, Text)
updatedOrderView =
  ( orderId updated
  , customer updated
  , quantity updated * unitPrice updated
  , expedited updated
  , note updated
  )
  where
    updated = baseOrder {quantity = orderQuantitySeed, expedited = True, note = "rush"}

-- A parameterized sum type with hand-written Functor and Foldable instances.
data Pair a = Leaf | Single a | Both a a

instance Functor Pair where
  fmap _ Leaf = Leaf
  fmap f (Single left) = Single (f left)
  fmap f (Both left right) = Both (f left) (f right)

instance Foldable Pair where
  foldr _ base Leaf = base
  foldr f base (Single left) = f left base
  foldr f base (Both left right) = f left (f right base)

functorFoldSum :: Int
functorFoldSum =
  sum (fmap (* 10) (Both pairLeftSeed pairRightSeed))
    + sum (fmap (+ 1) (Single singleSeed))
    + length (Leaf :: Pair Int)

-- Maybe/Either chains via >>= and traverse.
maybeChain :: Maybe Int
maybeChain =
  Just maybeLeftSeed >>= \left ->
    Just maybeRightSeed >>= \right ->
      if left * right > 40 then Just (left * right) else Nothing

eitherTraversed :: Either Text [Int]
eitherTraversed = traverse check traverseSeeds
  where
    check value
      | even value = Right (value * value)
      | otherwise = Left ("odd:" <> T.pack (show value))

eitherTraverseFailure :: Either Text [Int]
eitherTraverseFailure = traverse check traverseFailureSeeds
  where
    check value
      | even value = Right value
      | otherwise = Left ("odd:" <> T.pack (show value))

-- A small interpreter for an expression ADT: eval plus pretty-print to Text.
data Expr
  = Lit Int
  | Add Expr Expr
  | Mul Expr Expr
  | Neg Expr

sample :: Expr
sample = Add (Mul (Lit 3) (Add (Lit 1) (Lit 4))) (Neg (Lit 6))

evalExpr :: Int
evalExpr = go sample
  where
    go (Lit value) = value
    go (Add left right) = go left + go right
    go (Mul left right) = go left * go right
    go (Neg inner) = negate (go inner)

prettyExpr :: Text
prettyExpr = go sample
  where
    go (Lit value) = T.pack (show value)
    go (Add left right) = "(" <> go left <> " + " <> go right <> ")"
    go (Mul left right) = "(" <> go left <> " * " <> go right <> ")"
    go (Neg inner) = "-" <> go inner
