{-# LANGUAGE OverloadedStrings #-}

module UserTypesContract where

import Data.Text (Text)
import Data.Text qualified as T

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

defaultMethodPair :: (Text, Text)
defaultMethodPair = (summary Widget, summary Gadget)

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
    , unitPrice = 250
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
    updated = baseOrder {quantity = 5, expedited = True, note = "rush"}

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
functorFoldSum = sum (fmap (* 10) (Both 3 4)) + sum (fmap (+ 1) (Single 7)) + length (Leaf :: Pair Int)

-- Maybe/Either chains via >>= and traverse.
maybeChain :: Maybe Int
maybeChain =
  Just 6 >>= \left ->
    Just 7 >>= \right ->
      if left * right > 40 then Just (left * right) else Nothing

eitherTraversed :: Either Text [Int]
eitherTraversed = traverse check [2, 4, 6, 8]
  where
    check value
      | even value = Right (value * value)
      | otherwise = Left ("odd:" <> T.pack (show value))

eitherTraverseFailure :: Either Text [Int]
eitherTraverseFailure = traverse check [2, 5, 6]
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
