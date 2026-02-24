{-# LANGUAGE OverloadedStrings #-}
module TextSuite where

import qualified Data.Text as T
import Data.Text (Text)

-- ============================================================
-- Group 1: Construction (5)
-- ============================================================

-- Pack a String into Text
text_pack :: Text
text_pack = T.pack "hello world"

-- Empty text
text_empty :: Text
text_empty = T.empty

-- Singleton
text_singleton :: Text
text_singleton = T.singleton 'x'

-- Cons a char onto text
text_cons :: Text
text_cons = T.cons 'H' (T.pack "ello")

-- Snoc a char onto text
text_snoc :: Text
text_snoc = T.snoc (T.pack "hell") 'o'

-- ============================================================
-- Group 2: Basic queries (5)
-- ============================================================

-- Length
text_length :: Int
text_length = T.length (T.pack "hello")

-- Null check (empty)
text_null_empty :: Bool
text_null_empty = T.null T.empty

-- Null check (non-empty)
text_null_nonempty :: Bool
text_null_nonempty = T.null (T.pack "hi")

-- Head
text_head :: Char
text_head = T.head (T.pack "abc")

-- Last
text_last :: Char
text_last = T.last (T.pack "abc")

-- ============================================================
-- Group 3: Transformations (5)
-- ============================================================

-- Reverse
text_reverse :: Text
text_reverse = T.reverse (T.pack "hello")

-- ToUpper
text_toUpper :: Text
text_toUpper = T.toUpper (T.pack "hello")

-- ToLower
text_toLower :: Text
text_toLower = T.toLower (T.pack "HELLO")

-- Append
text_append :: Text
text_append = T.append (T.pack "hello") (T.pack " world")

-- Intercalate
text_intercalate :: Text
text_intercalate = T.intercalate (T.pack ", ") [T.pack "a", T.pack "b", T.pack "c"]

-- ============================================================
-- Group 4: Substrings / slicing (5)
-- ============================================================

-- Take
text_take :: Text
text_take = T.take 3 (T.pack "hello")

-- Drop
text_drop :: Text
text_drop = T.drop 2 (T.pack "hello")

-- TakeWhile
text_takeWhile :: Text
text_takeWhile = T.takeWhile (/= ' ') (T.pack "hello world")

-- DropWhile
text_dropWhile :: Text
text_dropWhile = T.dropWhile (/= ' ') (T.pack "hello world")

-- Tail
text_tail :: Text
text_tail = T.tail (T.pack "hello")

-- ============================================================
-- Group 5: Splitting (5)
-- ============================================================

-- SplitOn
text_splitOn :: [Text]
text_splitOn = T.splitOn (T.pack ",") (T.pack "a,b,c")

-- Words
text_words :: [Text]
text_words = T.words (T.pack "hello world  foo")

-- Lines
text_lines :: [Text]
text_lines = T.lines (T.pack "line1\nline2\nline3")

-- Unwords
text_unwords :: Text
text_unwords = T.unwords [T.pack "hello", T.pack "world"]

-- Unlines
text_unlines :: Text
text_unlines = T.unlines [T.pack "a", T.pack "b"]

-- ============================================================
-- Group 6: Searching (5)
-- ============================================================

-- IsPrefixOf
text_isPrefixOf :: Bool
text_isPrefixOf = T.isPrefixOf (T.pack "hel") (T.pack "hello")

-- IsSuffixOf
text_isSuffixOf :: Bool
text_isSuffixOf = T.isSuffixOf (T.pack "llo") (T.pack "hello")

-- IsInfixOf
text_isInfixOf :: Bool
text_isInfixOf = T.isInfixOf (T.pack "ell") (T.pack "hello")

-- Find
text_find :: Maybe Char
text_find = T.find (== 'l') (T.pack "hello")

-- Filter
text_filter :: Text
text_filter = T.filter (/= 'l') (T.pack "hello")

-- ============================================================
-- Group 7: Mapping and folding (5)
-- ============================================================

-- Map
text_map :: Text
text_map = T.map (\c -> if c == 'l' then 'r' else c) (T.pack "hello")

-- Foldr (count chars)
text_foldr :: Int
text_foldr = T.foldr (\_ n -> n + (1 :: Int)) (0 :: Int) (T.pack "hello")

-- Foldl (count chars)
text_foldl :: Int
text_foldl = T.foldl' (\n _ -> n + (1 :: Int)) (0 :: Int) (T.pack "hello")

-- ConcatMap
text_concatMap :: Text
text_concatMap = T.concatMap (\c -> T.pack [c, c]) (T.pack "abc")

-- Any
text_any :: Bool
text_any = T.any (== 'l') (T.pack "hello")

-- ============================================================
-- Group 8: Conversion (5)
-- ============================================================

-- Unpack
text_unpack :: String
text_unpack = T.unpack (T.pack "hello")

-- Unpack → length
text_unpack_length :: Int
text_unpack_length = length (T.unpack (T.pack "hello"))

-- Show text (goes through show instance)
text_show :: String
text_show = show (T.pack "hello")

-- Pack → unpack roundtrip
text_roundtrip :: Bool
text_roundtrip = T.unpack (T.pack "hello") == "hello"

-- Compare two texts
text_compare :: Bool
text_compare = T.pack "abc" < T.pack "abd"

-- ============================================================
-- Group 9: Numeric conversions (for Aeson path) (5)
-- ============================================================

-- Read int from text (manual)
text_read_int :: Int
text_read_int = read (T.unpack (T.pack "42")) :: Int

-- Show int, pack to text
text_show_int :: Text
text_show_int = T.pack (show (42 :: Int))

-- Pack → length → equality
text_length_eq :: Bool
text_length_eq = T.length (T.pack "hello") == 5

-- Replicate
text_replicate :: Text
text_replicate = T.replicate 3 (T.pack "ab")

-- Strip whitespace
text_strip :: Text
text_strip = T.strip (T.pack "  hello  ")

-- ============================================================
-- Group 10: Composition patterns for Aeson (5)
-- ============================================================

-- Build a key-value pair string (simulates JSON field)
text_kv :: Text
text_kv = T.concat [T.pack "\"name\"", T.pack ": ", T.pack "\"alice\""]

-- Join a list of texts with separator
text_join :: Text
text_join = T.intercalate (T.pack ", ") (map (\n -> T.pack (show (n :: Int))) [1,2,3])

-- Nested pack/unpack
text_nested :: Bool
text_nested = T.pack (T.unpack (T.pack "hello")) == T.pack "hello"

-- Replace
text_replace :: Text
text_replace = T.replace (T.pack "world") (T.pack "there") (T.pack "hello world")

-- All (check all chars satisfy predicate)
text_all :: Bool
text_all = T.all (\c -> c >= 'a' && c <= 'z') (T.pack "hello")
