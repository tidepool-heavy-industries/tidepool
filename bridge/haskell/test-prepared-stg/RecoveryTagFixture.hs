module RecoveryTagFixture where

-- Native interface facts for partial original preparation. NOINLINE keeps the
-- public boundaries visible to the independent GHC compiler.
data Leaf = Leaf
data StrictLeaf = StrictLeaf !Leaf
data LazyLeaf = LazyLeaf Leaf
data StrictFunction = StrictFunction !(Int -> Maybe Int)

leaf :: Leaf
leaf = Leaf
{-# NOINLINE leaf #-}
strictLeaf :: StrictLeaf
strictLeaf = StrictLeaf leaf
{-# NOINLINE strictLeaf #-}
lazyLeaf :: LazyLeaf
lazyLeaf = LazyLeaf leaf
{-# NOINLINE lazyLeaf #-}
taggedFunction :: Int -> Maybe Int
taggedFunction x = Just x
{-# NOINLINE taggedFunction #-}
unknownFunction :: Maybe Int -> Maybe Int
unknownFunction x = x
{-# NOINLINE unknownFunction #-}
unknownFieldFunction :: Int -> Maybe Int
unknownFieldFunction x = unknownFunction (Just x)
{-# NOINLINE unknownFieldFunction #-}
indirectFunction :: Int -> Maybe Int
indirectFunction x = taggedFunction x
{-# NOINLINE indirectFunction #-}
strictFunction :: StrictFunction
strictFunction = StrictFunction taggedFunction
{-# NOINLINE strictFunction #-}
unknownStrictFunction :: StrictFunction
unknownStrictFunction = StrictFunction unknownFieldFunction
{-# NOINLINE unknownStrictFunction #-}
indirectStrictFunction :: StrictFunction
indirectStrictFunction = StrictFunction indirectFunction
{-# NOINLINE indirectStrictFunction #-}
ordinaryCall :: Int -> Maybe Int
ordinaryCall x = taggedFunction x
{-# NOINLINE ordinaryCall #-}
unknownLeafIdentity :: Leaf -> Leaf
unknownLeafIdentity x = x
{-# NOINLINE unknownLeafIdentity #-}
unknownLeaf :: Leaf
unknownLeaf = unknownLeafIdentity Leaf
{-# NOINLINE unknownLeaf #-}
