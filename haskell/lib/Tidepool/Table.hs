{-# LANGUAGE BangPatterns, NoImplicitPrelude, OverloadedStrings #-}
-- | CSV/TSV parsing and table rendering.
--
-- Available in MCP via: @import Tidepool.Table@
module Tidepool.Table
  ( -- * Parsing
    parseCsv
  , parseTsv
  , parseDelimited
    -- * Rendering
  , renderTable
  , renderTableWith
    -- * Column operations
  , column
  , sortByColumn
  , filterByColumn
  ) where

import Prelude
  ( Int, Char, Bool(..), Maybe(..), String, Ordering(..)
  , Eq(..), Ord(..), Num(..), Show
  , Semigroup(..), Monoid(..)
  , ($), (.), otherwise, not, (&&), (||), negate, fst, snd
  , map, filter, foldl, foldl', foldr
  , null, error, fromIntegral
  , zip, length, replicate, reverse, concatMap
  )
import Data.Text (Text)
import qualified Data.Text as T

-- ---------------------------------------------------------------------------
-- Parsing
-- ---------------------------------------------------------------------------

-- | Parse CSV text into rows of fields.
-- Handles simple CSV (no quoting). Splits on commas and newlines.
parseCsv :: Text -> [[Text]]
parseCsv = parseDelimited ','

-- | Parse TSV text into rows of fields.
parseTsv :: Text -> [[Text]]
parseTsv = parseDelimited '\t'

-- | Parse text delimited by the given character into rows of fields.
parseDelimited :: Char -> Text -> [[Text]]
parseDelimited delim t =
  let ls = filter (not . T.null) (T.lines t)
  in  map (T.splitOn (T.singleton delim)) ls

-- ---------------------------------------------------------------------------
-- Rendering
-- ---------------------------------------------------------------------------

-- | Render a list of rows as an aligned table with pipe separators.
--
-- >>> renderTable [["Name","Age"],["Alice","30"],["Bob","25"]]
-- "| Name  | Age |"
-- "| Alice | 30  |"
-- "| Bob   | 25  |"
renderTable :: [[Text]] -> Text
renderTable = renderTableWith '|' ' '

-- | Render a table with custom separator and padding characters.
renderTableWith :: Char -> Char -> [[Text]] -> Text
renderTableWith sep pad rows =
  let widths = colWidths rows
      rendered = map (renderRow sep pad widths) rows
  in  T.unlines rendered

colWidths :: [[Text]] -> [Int]
colWidths [] = []
colWidths rows =
  let ncols = maxList 0 (map length rows)
      getCol i = map (safeIndex i) rows
      safeIndex i xs = case safeDrop i xs of
        []    -> T.empty
        (x:_) -> x
  in  map (\i -> maxList 0 (map T.length (getCol i))) (enumFromTo 0 (ncols - 1))

maxList :: Int -> [Int] -> Int
maxList d [] = d
maxList _ (x:xs) = foldl' (\a b -> if a >= b then a else b) x xs

renderRow :: Char -> Char -> [Int] -> [Text] -> Text
renderRow sep pad widths fields =
  let sepT = T.singleton sep
      padT = T.singleton pad
      cells = zipPad widths fields
      rendered = map (\(w, f) -> padT <> padRight w pad f <> padT) cells
  in  sepT <> T.intercalate sepT rendered <> sepT

zipPad :: [Int] -> [Text] -> [(Int, Text)]
zipPad [] _ = []
zipPad (w:ws) [] = (w, T.empty) : zipPad ws []
zipPad (w:ws) (f:fs) = (w, f) : zipPad ws fs

padRight :: Int -> Char -> Text -> Text
padRight w pad t
  | T.length t >= w = t
  | otherwise = t <> T.replicate (w - T.length t) (T.singleton pad)

enumFromTo :: Int -> Int -> [Int]
enumFromTo lo hi
  | lo > hi   = []
  | otherwise = lo : enumFromTo (lo + 1) hi

safeDrop :: Int -> [a] -> [a]
safeDrop 0 xs     = xs
safeDrop _ []     = []
safeDrop !n (_:xs) = safeDrop (n - 1) xs

-- ---------------------------------------------------------------------------
-- Column operations
-- ---------------------------------------------------------------------------

-- | Extract a column by index (0-based) from parsed rows.
column :: Int -> [[Text]] -> [Text]
column i = map (safeIdx i)
  where
    safeIdx n xs = case safeDrop n xs of
      []    -> T.empty
      (x:_) -> x

-- | Sort rows by a column index (0-based), using Text ordering.
-- First row (header) stays in place if present.
sortByColumn :: Int -> [[Text]] -> [[Text]]
sortByColumn _ [] = []
sortByColumn i (header:rows) = header : sortBy (comparing (safeIdx i)) rows
  where
    safeIdx n xs = case safeDrop n xs of
      []    -> T.empty
      (x:_) -> x

-- | Filter rows where the column value satisfies a predicate.
-- First row (header) is always kept.
filterByColumn :: Int -> (Text -> Bool) -> [[Text]] -> [[Text]]
filterByColumn _ _ [] = []
filterByColumn i p (header:rows) = header : filter (\r -> p (safeIdx i r)) rows
  where
    safeIdx n xs = case safeDrop n xs of
      []    -> T.empty
      (x:_) -> x

-- ---------------------------------------------------------------------------
-- Local sort (avoid importing from Prelude to keep module self-contained)
-- ---------------------------------------------------------------------------

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

comparing :: Ord b => (a -> b) -> a -> a -> Ordering
comparing f x y = compare (f x) (f y)
  where compare a b | a == b    = EQ
                    | a <= b    = LT
                    | otherwise = GT
