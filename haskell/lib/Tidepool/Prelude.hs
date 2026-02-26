{-# LANGUAGE BangPatterns, NoImplicitPrelude #-}
-- | Self-contained prelude for Tidepool user code.
--
-- With NoImplicitPrelude in the MCP template, this is the single import.
-- Nothing from base Prelude is re-exported — every function is either
-- defined here or explicitly re-exported from a known-safe base module.
module Tidepool.Prelude
  ( -- * Renderable marker
    Renderable
    -- * Types (re-exported from base)
  , Int, Integer, Word, Char, Bool(..), Double, Float
  , String, Ordering(..), Maybe(..), Either(..)
  , IO
    -- * Text type (re-exported from Data.Text)
  , Text
  , pack, unpack
  , toUpper, toLower
  , strip
  , splitOn
  , replace
  , isSuffixOf, isInfixOf
    -- * Text versions of words/lines
  , words, lines, unwords, unlines
    -- * Typeclasses (re-exported from base)
  , Eq(..), Ord(..), Num(..), Integral(..), Real, Fractional, Show
  , fromIntegral, realToFrac
  , Functor(..), Applicative(..), Monad(..)
    -- * show (Text-returning shadow)
  , show
    -- * Basic functions (re-exported from base)
  , id, const, flip, (.), ($), ($!)
  , not, (&&), (||), otherwise, seq
  , fst, snd, curry, uncurry
  , error, undefined
    -- * List operations
  , map, filter, foldl, foldl', foldr
  , null
  , take, drop, zip, zipWith, unzip
  , lookup, elem, notElem
  , any, all, and, or
  , sum, product, minimum, maximum
  , concat, iterate, repeat, cycle
  , scanl, scanr
    -- * Self-contained list operations
  , reverse
  , splitAt
  , span
  , break
  , init
  , nub
  , nubBy
  , sort
  , sortBy
  , concatMap
  , append
  , (++)
  , dropWhile
  , length
  , replicate
  , intercalate
  , isPrefixOf
  , intersperse
    -- * Monadic combinators
  , mapM, mapM_, sequence, sequence_
  , when, unless, void, join, guard
  , forM, forM_
  , (=<<), (>=>), (<=<)
  , foldM, foldM_
    -- * Maybe/Either utilities
  , maybe, fromMaybe, isJust, isNothing, catMaybes, mapMaybe
  , either
    -- * Partial functions (use with care)
  , head
  , tail
  , last
    -- * Numeric utilities
  , even, odd
    -- * Char/Enum
  , ord, chr, fromEnum
  ) where

import Prelude
  ( Int, Integer, Word, Char, Bool(..), Double, Float
  , String, Ordering(..), Maybe(..), Either(..)
  , IO
  , Eq(..), Ord(..), Num(..), Integral(..), Real, Fractional, Show
  , fromIntegral, realToFrac, even, odd
  , Functor(..), Applicative(..), Monad(..)
  , id, const, flip, (.), ($), ($!)
  , not, (&&), (||), otherwise, seq
  , fst, snd, curry, uncurry
  , error, undefined
  , maybe, either
  , map, filter, foldl, foldr
  , take, drop, zip, zipWith, unzip
  , lookup, elem, notElem
  , any, all, and, or
  , sum, product, minimum, maximum
  , concat, iterate, repeat, cycle
  , scanl, scanr
  , negate, quot, rem
  , compare
  , fromEnum
  , mapM, mapM_, sequence, sequence_
  )
import qualified Prelude as P (show)
import Data.Text (Text)
import qualified Data.Text as T
  ( pack, unpack
  , toUpper, toLower
  , strip
  , splitOn
  , replace
  , isSuffixOf, isInfixOf
  , words, lines, unwords, unlines
  )
import Data.Char (ord, chr)
import Data.Maybe (fromMaybe, isJust, isNothing, catMaybes, mapMaybe)
import Data.List (foldl')
import Control.Monad
  ( when, unless, void, join, guard
  , forM, forM_
  , (=<<), (>=>), (<=<)
  , foldM, foldM_
  )

-- | Marker typeclass for types whose runtime values can be rendered to JSON
-- by the Rust-side value_to_json renderer. Use @pure x@ to return values
-- instead of @send (Print (show x))@.
class Renderable a
instance Renderable Int
instance Renderable Integer
instance Renderable Word
instance Renderable Char
instance Renderable Double
instance Renderable Float
instance Renderable Bool
instance Renderable ()
instance Renderable Text
instance Renderable a => Renderable [a]
instance Renderable a => Renderable (Maybe a)
instance (Renderable a, Renderable b) => Renderable (a, b)
instance (Renderable a, Renderable b, Renderable c) => Renderable (a, b, c)
instance (Renderable a, Renderable b, Renderable c, Renderable d) => Renderable (a, b, c, d)

-- | Text-returning show: @show x@ gives @Text@ instead of @String@.
show :: Show a => a -> Text
show = T.pack . P.show

-- Re-export Data.Text functions unqualified (non-colliding names)
pack :: String -> Text
pack = T.pack

unpack :: Text -> String
unpack = T.unpack

toUpper :: Text -> Text
toUpper = T.toUpper

toLower :: Text -> Text
toLower = T.toLower

strip :: Text -> Text
strip = T.strip

splitOn :: Text -> Text -> [Text]
splitOn = T.splitOn

replace :: Text -> Text -> Text -> Text
replace = T.replace

isSuffixOf :: Text -> Text -> Bool
isSuffixOf = T.isSuffixOf

isInfixOf :: Text -> Text -> Bool
isInfixOf = T.isInfixOf

words :: Text -> [Text]
words = T.words

lines :: Text -> [Text]
lines = T.lines

unwords :: [Text] -> Text
unwords = T.unwords

unlines :: [Text] -> Text
unlines = T.unlines

-- | Append two lists.
append :: [a] -> [a] -> [a]
append []     ys = ys
append (x:xs) ys = x : append xs ys
{-# INLINE append #-}

(++) :: [a] -> [a] -> [a]
(++) = append
{-# INLINE (++) #-}
infixr 5 ++

-- | Check if a list is empty.
null :: [a] -> Bool
null [] = True
null _  = False
{-# INLINE null #-}

-- | Reverse a list.
reverse :: [a] -> [a]
reverse = go []
  where
    go :: [a] -> [a] -> [a]
    go acc []     = acc
    go acc (x:xs) = go (x:acc) xs
{-# INLINE reverse #-}

-- | Split a list at position n.
splitAt :: Int -> [a] -> ([a], [a])
splitAt n xs = go n xs
  where
    go :: Int -> [a] -> ([a], [a])
    go 0 ys      = ([], ys)
    go _ []      = ([], [])
    go !m (y:ys) = let (as, bs) = go (m - 1) ys in (y:as, bs)
{-# INLINE splitAt #-}

-- | Take the longest prefix satisfying a predicate.
span :: (a -> Bool) -> [a] -> ([a], [a])
span _ []     = ([], [])
span p xs@(x:xs')
  | p x       = let (ys, zs) = span p xs' in (x:ys, zs)
  | otherwise  = ([], xs)
{-# INLINE span #-}

-- | Take the longest prefix NOT satisfying a predicate.
break :: (a -> Bool) -> [a] -> ([a], [a])
break _ []     = ([], [])
break p xs@(x:xs')
  | p x       = ([], xs)
  | otherwise  = let (ys, zs) = break p xs' in (x:ys, zs)
{-# INLINE break #-}

-- | Drop the longest prefix satisfying a predicate.
dropWhile :: (a -> Bool) -> [a] -> [a]
dropWhile _ []     = []
dropWhile p (x:xs)
  | p x       = dropWhile p xs
  | otherwise  = x : xs
{-# INLINE dropWhile #-}

-- | All elements except the last. Returns [] for empty input.
init :: [a] -> [a]
init []     = []
init [_]    = []
init (x:xs) = x : init xs
{-# INLINE init #-}

-- | Remove duplicate elements (preserving first occurrence).
nub :: Eq a => [a] -> [a]
nub = go []
  where
    go :: Eq a => [a] -> [a] -> [a]
    go _ []     = []
    go seen (x:xs)
      | elemOf x seen = go seen xs
      | otherwise      = x : go (x:seen) xs
    elemOf :: Eq a => a -> [a] -> Bool
    elemOf _ []     = False
    elemOf y (z:zs) = y == z || elemOf y zs
{-# INLINABLE nub #-}

-- | Remove duplicates using a custom equality function.
nubBy :: (a -> a -> Bool) -> [a] -> [a]
nubBy eq = go []
  where
    go _ []     = []
    go seen (x:xs)
      | elemOf x seen = go seen xs
      | otherwise      = x : go (x:seen) xs
    elemOf _ []     = False
    elemOf y (z:zs) = eq y z || elemOf y zs
{-# INLINABLE nubBy #-}

-- | Sort a list using merge sort.
sort :: Ord a => [a] -> [a]
sort = mergeSort
  where
    mergeSort :: Ord a => [a] -> [a]
    mergeSort []  = []
    mergeSort [x] = [x]
    mergeSort xs  = let (as, bs) = halve xs
                    in merge (mergeSort as) (mergeSort bs)
    halve :: [a] -> ([a], [a])
    halve []       = ([], [])
    halve [x]      = ([x], [])
    halve (x:y:zs) = let (as, bs) = halve zs in (x:as, y:bs)
    merge :: Ord a => [a] -> [a] -> [a]
    merge [] ys = ys
    merge xs [] = xs
    merge (x:xs) (y:ys)
      | x <= y    = x : merge xs (y:ys)
      | otherwise  = y : merge (x:xs) ys
{-# INLINABLE sort #-}

-- | Sort using a custom comparison function.
sortBy :: (a -> a -> Ordering) -> [a] -> [a]
sortBy cmp = mergeSort
  where
    mergeSort []  = []
    mergeSort [x] = [x]
    mergeSort xs  = let (as, bs) = halve xs
                    in merge (mergeSort as) (mergeSort bs)
    halve []       = ([], [])
    halve [x]      = ([x], [])
    halve (x:y:zs) = let (as, bs) = halve zs in (x:as, y:bs)
    merge [] ys = ys
    merge xs [] = xs
    merge (x:xs) (y:ys)
      | cmp x y /= GT = x : merge xs (y:ys)
      | otherwise      = y : merge (x:xs) ys
{-# INLINABLE sortBy #-}

-- | Map a function over a list and concatenate results.
concatMap :: (a -> [b]) -> [a] -> [b]
concatMap _ []     = []
concatMap f (x:xs) = f x `append` concatMap f xs
{-# INLINE concatMap #-}

-- | Length of a list.
length :: [a] -> Int
length = go 0
  where
    go :: Int -> [a] -> Int
    go !acc []     = acc
    go !acc (_:xs) = go (acc + 1) xs
{-# INLINE length #-}

-- | Build a list of n copies of a value.
replicate :: Int -> a -> [a]
replicate n x = go n
  where
    go 0 = []
    go !m = x : go (m - 1)
{-# INLINE replicate #-}

-- | Insert a list between every element of a list of lists.
intercalate :: [a] -> [[a]] -> [a]
intercalate _   []     = []
intercalate _   [x]    = x
intercalate sep (x:xs) = x `append` (sep `append` intercalate sep xs)
{-# INLINE intercalate #-}

-- | Is the first list a prefix of the second?
isPrefixOf :: Eq a => [a] -> [a] -> Bool
isPrefixOf []     _      = True
isPrefixOf _      []     = False
isPrefixOf (x:xs) (y:ys) = x == y && isPrefixOf xs ys
{-# INLINABLE isPrefixOf #-}

-- | Insert an element between every pair of elements.
intersperse :: a -> [a] -> [a]
intersperse _   []     = []
intersperse _   [x]    = [x]
intersperse sep (x:xs) = x : sep : intersperse sep xs
{-# INLINE intersperse #-}

-- | Extract the first element. Partial: errors on empty list.
head :: [a] -> a
head (x:_) = x
head []    = error "head: empty list"
{-# INLINE head #-}

-- | Extract all elements after the head. Partial: errors on empty list.
tail :: [a] -> [a]
tail (_:xs) = xs
tail []     = error "tail: empty list"
{-# INLINE tail #-}

-- | Extract the last element. Partial: errors on empty list.
last :: [a] -> a
last [x]    = x
last (_:xs) = last xs
last []     = error "last: empty list"
{-# INLINE last #-}
