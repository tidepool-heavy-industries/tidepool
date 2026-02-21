{-# LANGUAGE BangPatterns #-}
-- | Bundled prelude providing source definitions for common functions
-- whose GHC base library workers lack unfoldings in .hi files.
--
-- All definitions are self-contained — no base library functions are
-- used that lack unfoldings. This ensures a fully closed Core IR.
module Tidepool.Prelude
  ( -- * Renderable marker
    Renderable
    -- * List operations
  , reverse
  , splitAt
  , span
  , break
  , init
  , words
  , lines
  , unlines
  , unwords
  , nub
  , sort
  , concatMap
  , append
  , dropWhile
    -- * Numeric
  , showInt
  , length
    -- * String comparison
  , compareString
  ) where

import Prelude hiding
  ( reverse, splitAt, span, break, init
  , words, lines, unlines, unwords
  , concatMap, dropWhile, length, (++) )

-- | Marker typeclass for types whose runtime values can be rendered to JSON
-- by the Rust-side value_to_json renderer. Use @pure x@ to return values
-- instead of @send (Print (show x))@.
class Renderable a
instance Renderable Int
instance Renderable Word
instance Renderable Char
instance Renderable Double
instance Renderable Float
instance Renderable Bool
instance Renderable ()
instance Renderable a => Renderable [a]
instance Renderable a => Renderable (Maybe a)
instance (Renderable a, Renderable b) => Renderable (a, b)
instance (Renderable a, Renderable b, Renderable c) => Renderable (a, b, c)
instance (Renderable a, Renderable b, Renderable c, Renderable d) => Renderable (a, b, c, d)

-- | Append two lists. Self-contained replacement for (++).
append :: [a] -> [a] -> [a]
append []     ys = ys
append (x:xs) ys = x : append xs ys
{-# INLINE append #-}

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

-- | Split a string into words separated by whitespace.
words :: String -> [String]
words s = case dropWhile isSpace s of
  [] -> []
  s' -> let (w, s'') = break isSpace s' in w : words s''
  where
    isSpace :: Char -> Bool
    isSpace c = c == ' ' || c == '\t' || c == '\n' || c == '\r'
{-# INLINE words #-}

-- | Split a string into lines separated by newline characters.
lines :: String -> [String]
lines [] = []
lines s  = let (l, s') = break eqNl s
           in l : case s' of
                    []      -> []
                    (_:s'') -> lines s''
  where
    eqNl :: Char -> Bool
    eqNl c = c == '\n'
{-# INLINE lines #-}

-- | Join lines with newline separators.
unlines :: [String] -> String
unlines []     = []
unlines (l:ls) = l `append` ('\n' : unlines ls)
{-# INLINE unlines #-}

-- | Join words with space separators.
unwords :: [String] -> String
unwords []     = []
unwords [w]    = w
unwords (w:ws) = w `append` (' ' : unwords ws)
{-# INLINE unwords #-}

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

-- | Map a function over a list and concatenate results.
concatMap :: (a -> [b]) -> [a] -> [b]
concatMap _ []     = []
concatMap f (x:xs) = f x `append` concatMap f xs
{-# INLINE concatMap #-}

-- | Length of a list. Self-contained replacement for Prelude's length.
length :: [a] -> Int
length = go 0
  where
    go :: Int -> [a] -> Int
    go !acc []     = acc
    go !acc (_:xs) = go (acc + 1) xs
{-# INLINE length #-}

-- | Convert an Int to its decimal String representation.
-- Uses quot/rem separately to avoid quotRemInt# (unboxed tuple primop).
showInt :: Int -> String
showInt n
  | n < (0 :: Int)  = '-' : showPos (negate n)
  | n == (0 :: Int) = "0"
  | otherwise       = showPos n
  where
    showPos :: Int -> String
    showPos m
      | m == (0 :: Int) = ""
      | otherwise       = showPos (quot m (10 :: Int)) `append` [digitToChar (rem m (10 :: Int))]
    digitToChar :: Int -> Char
    digitToChar d = case d of
      0 -> '0'; 1 -> '1'; 2 -> '2'; 3 -> '3'; 4 -> '4'
      5 -> '5'; 6 -> '6'; 7 -> '7'; 8 -> '8'; 9 -> '9'
      _ -> '?'
{-# INLINE showInt #-}

-- | Lexicographic comparison of Strings. Self-contained replacement for
-- compare on [Char] that avoids GHC's $fOrdList specialization.
compareString :: String -> String -> Ordering
compareString []     []     = EQ
compareString []     (_:_)  = LT
compareString (_:_)  []     = GT
compareString (x:xs) (y:ys)
  | x < y    = LT
  | x > y    = GT
  | otherwise = compareString xs ys
{-# INLINE compareString #-}

