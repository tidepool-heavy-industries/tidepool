{-# LANGUAGE BangPatterns, NoImplicitPrelude, OverloadedStrings #-}
-- | Advanced text utilities for code generation and formatting.
--
-- All functions operate on Data.Text. Available in MCP via:
-- @import Tidepool.Text@
module Tidepool.Text
  ( -- * Case conversion
    camelToSnake
  , snakeToCamel
  , capitalize
  , titleCase
    -- * Formatting
  , center
  , padLeft
  , padRight
  , indent
  , dedent
  , wrap
    -- * Transformations
  , slugify
  , truncateText
  ) where

import Prelude
  ( Int, Char, Bool(..), Maybe(..), String
  , Eq(..), Ord(..), Num(..), Integral(..)
  , Semigroup(..), Monoid(..)
  , ($), (.), otherwise, not, (&&), (||), negate, fst
  , map, filter, foldl, foldr, foldl'
  , null, error, fromIntegral, reverse, concatMap
  )
import Data.Text (Text)
import qualified Data.Text as T
import Data.Char (ord, chr)

-- ---------------------------------------------------------------------------
-- Internal char helpers (avoid Data.Char dictionary)
-- ---------------------------------------------------------------------------

isUpper :: Char -> Bool
isUpper c = c >= 'A' && c <= 'Z'

isLower :: Char -> Bool
isLower c = c >= 'a' && c <= 'z'

isAlpha :: Char -> Bool
isAlpha c = isUpper c || isLower c

isAlphaNum :: Char -> Bool
isAlphaNum c = isAlpha c || (c >= '0' && c <= '9')

toLowerC :: Char -> Char
toLowerC c
  | isUpper c = chr (ord c + 32)
  | otherwise = c

toUpperC :: Char -> Char
toUpperC c
  | isLower c = chr (ord c - 32)
  | otherwise = c

-- ---------------------------------------------------------------------------
-- Case conversion
-- ---------------------------------------------------------------------------

-- | Convert camelCase or PascalCase to snake_case.
--
-- >>> camelToSnake "helloWorld"
-- "hello_world"
-- >>> camelToSnake "HTTPServer"
-- "h_t_t_p_server"
camelToSnake :: Text -> Text
camelToSnake = T.pack . go . T.unpack
  where
    go [] = []
    go (c:cs)
      | isUpper c = '_' : toLowerC c : go cs
      | otherwise = c : go cs

-- | Convert snake_case to camelCase.
--
-- >>> snakeToCamel "hello_world"
-- "helloWorld"
snakeToCamel :: Text -> Text
snakeToCamel t = case T.splitOn "_" t of
  []     -> T.empty
  (w:ws) -> T.concat (w : map capitalize ws)

-- | Capitalize the first character of a Text.
--
-- >>> capitalize "hello"
-- "Hello"
capitalize :: Text -> Text
capitalize t = case T.uncons t of
  Nothing      -> T.empty
  Just (c, cs) -> T.cons (toUpperC c) cs

-- | Capitalize each word in a Text.
--
-- >>> titleCase "hello world"
-- "Hello World"
titleCase :: Text -> Text
titleCase = T.unwords . map capitalize . T.words

-- ---------------------------------------------------------------------------
-- Formatting
-- ---------------------------------------------------------------------------

-- | Center text in a field of given width, padding with the given character.
center :: Int -> Char -> Text -> Text
center w pad t
  | T.length t >= w = t
  | otherwise =
      let total = w - T.length t
          lpad  = total `div` 2
          rpad  = total - lpad
      in  T.replicate lpad (T.singleton pad) <> t <> T.replicate rpad (T.singleton pad)

-- | Pad text on the left to a given width.
padLeft :: Int -> Char -> Text -> Text
padLeft w pad t
  | T.length t >= w = t
  | otherwise = T.replicate (w - T.length t) (T.singleton pad) <> t

-- | Pad text on the right to a given width.
padRight :: Int -> Char -> Text -> Text
padRight w pad t
  | T.length t >= w = t
  | otherwise = t <> T.replicate (w - T.length t) (T.singleton pad)

-- | Indent every line of text by n spaces.
indent :: Int -> Text -> Text
indent n t = T.unlines (map (prefix <>) (T.lines t))
  where prefix = T.replicate n " "

-- | Remove common leading whitespace from all non-empty lines.
dedent :: Text -> Text
dedent t =
  let ls = T.lines t
      nonEmpty = filter (not . T.null . T.stripStart) ls
      minIndent = case nonEmpty of
        [] -> 0
        _  -> foldl' (\acc l -> min' acc (countLeading l)) 999999 nonEmpty
  in  T.unlines (map (T.drop minIndent) ls)
  where
    countLeading = T.length . T.takeWhile (== ' ')
    min' a b = if a <= b then a else b

-- | Wrap text to a given line width at word boundaries.
wrap :: Int -> Text -> Text
wrap w = T.unlines . concatMap (wrapLine w) . T.lines
  where
    wrapLine :: Int -> Text -> [Text]
    wrapLine width line
      | T.length line <= width = [line]
      | otherwise = go width (T.words line) [] 0
    go _ [] acc _ = [T.unwords (reverse acc)]
    go width (word:ws) acc lineLen
      | null acc  = go width ws [word] (T.length word)
      | lineLen + 1 + T.length word > width =
          T.unwords (reverse acc) : go width (word:ws) [] 0
      | otherwise = go width ws (word:acc) (lineLen + 1 + T.length word)

-- ---------------------------------------------------------------------------
-- Transformations
-- ---------------------------------------------------------------------------

-- | Convert text to a URL-friendly slug (lowercase, hyphens for spaces/punctuation).
--
-- >>> slugify "Hello, World!"
-- "hello-world"
slugify :: Text -> Text
slugify = collapseHyphens . T.map toLowerOrHyphen . T.strip
  where
    toLowerOrHyphen c
      | isAlphaNum c = toLowerC c
      | otherwise    = '-'
    collapseHyphens = T.intercalate "-" . filter (not . T.null) . T.splitOn "-"

-- | Truncate text to n characters, appending "..." if truncated.
truncateText :: Int -> Text -> Text
truncateText n t
  | T.length t <= n = t
  | n <= 3          = T.take n t
  | otherwise       = T.take (n - 3) t <> "..."
