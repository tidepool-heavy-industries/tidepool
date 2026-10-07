module HomeSelf (Node (..), singleton, headValue) where

-- The native constructor and its functions share one home module owner.
data Node = End | Node Int Node

singleton :: Int -> Node
singleton value = Node value End
{-# NOINLINE singleton #-}

headValue :: Node -> Int
headValue End = 0
headValue (Node value _) = value
{-# NOINLINE headValue #-}
