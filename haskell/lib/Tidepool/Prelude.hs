{-# LANGUAGE BangPatterns, NoImplicitPrelude #-}
-- | Self-contained prelude for Tidepool user code.
--
-- With NoImplicitPrelude in the MCP template, this is the single import.
-- Nothing from base Prelude is re-exported — every function is either
-- defined here or explicitly re-exported from a known-safe base module.
module Tidepool.Prelude
  ( -- * Types (re-exported from base)
    Int, Integer, Word, Char, Bool(..), Double, Float
  , String, Ordering(..), Maybe(..), Either(..)
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
  , Eq(..), Ord(..), Num(..), Integral(..), Real, Fractional(..), Floating(..), Show
  , Semigroup(..), Monoid(..)
  , fromIntegral, realToFrac, truncate, ceiling, floor, round
  , Functor(..), Applicative(..), Monad(..)
  , (<$>)
    -- * show (Text-returning shadow)
  , show, showT
  , showDouble
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
    -- * Additional list combinators
  , find
  , partition
  , groupBy
  , takeWhile
  , tails
  , unfoldr
  , mapAccumL
  , transpose
  , genericLength
  , zipWith3
  , zipWith4
    -- * Function combinators
  , on
  , comparing
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
    -- * Text-to-number parsing
  , parseIntM, parseInt, parseDoubleM, parseDouble
    -- * Char predicates & conversions
  , ord, chr, fromEnum
  , isDigit, isAlpha, isAlphaNum, isSpace, isUpper, isLower
  , digitToInt, toLowerChar, toUpperChar
    -- * Indexed list operations (safe alternatives to [0..])
  , zipWithIndex, imap, enumFromTo
    -- * Monomorphic numeric helpers
  , abs', signum', min', max'
    -- * Additional list combinators (P2)
  , elemIndex, findIndex
  , zip3, unzip3
    -- * Map/Set types
  , Map, Set
    -- * JSON (Tidepool.Aeson — vendored, construction-only)
  , Value(..), Key, object, (.=), toJSON
  , ToJSON
    -- * JSON lenses (Tidepool.Aeson.Lens + Control.Lens)
  , key, nth, _String, _Number, _Bool, _Array, _Object, _Int, _Double
  , members, values, _Null
  , preview, toListOf, (^?), (^..), (&), (.~), (%~), to, _Just, traverse
    -- * JSON Value helpers
  , (?.), lookupKey, asText, asInt, asDouble, asBool, asArray, asObject
    -- * Map operations (qualified via Map prefix)
  , Map.fromList, Map.toList, Map.insert, Map.delete
  , Map.member, Map.size, Map.keys, Map.elems
  , Map.union, Map.intersection, Map.difference
  , Map.foldlWithKey', Map.foldrWithKey
  , Map.mapKeys, Map.mapWithKey, Map.filterWithKey
  , Map.singleton, Map.empty
  , Map.findWithDefault, Map.adjust
  , Map.unionWith, Map.intersectionWith
    -- * Set type (use qualified Set.xxx via preamble's `import qualified Data.Set as Set`)
  ) where

import Prelude
  ( Int, Integer, Word, Char, Bool(..), Double, Float
  , String, Ordering(..), Maybe(..), Either(..)
  , Eq(..), Ord(..), Num(..), Integral(..), Real, Fractional(..), Floating(..), Show
  , Semigroup(..), Monoid(..)
  , fromIntegral, realToFrac, truncate, ceiling, floor
  , Functor(..), Applicative(..), Monad(..)
  , (<$>)
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
import Data.Char (ord, chr)
import Data.Maybe (fromMaybe, isJust, isNothing, catMaybes, mapMaybe)
import Data.List (foldl', find, partition, groupBy, takeWhile, tails, unfoldr, mapAccumL, transpose, genericLength)
import Data.Map.Strict (Map)
import Data.Set (Set)
import Control.Monad
  ( when, unless, void, join, guard
  , forM, forM_
  , (=<<), (>=>), (<=<)
  , foldM, foldM_
  )
import Tidepool.Aeson (Value(..), Key, object, (.=), toJSON, ToJSON, fromText)
import Tidepool.Aeson.Lens (key, nth, _String, _Number, _Bool, _Array, _Object, _Int, _Double, members, values, _Null)
import Control.Lens (preview, toListOf, (^?), (^..), (&), (.~), (%~), to, _Just, traverse)
import qualified Data.Map.Strict as Map

-- | show for Double, bypassing GHC's Integer-based floatToDigits.
-- The body is a fallback that should never run — Translate.hs intercepts
-- calls to showDouble and emits a ShowDoubleAddr primop instead.
-- The Double arg must be used to prevent GHC worker-wrapper from dropping it.
{-# NOINLINE showDouble #-}
showDouble :: Double -> String
showDouble d = case d of !_ -> error "showDouble: should be intercepted by Translate"

-- | Text-returning show: @show x@ gives @Text@ instead of @String@.
show :: Show a => a -> Text
show = T.pack . P.show

-- | Alias for 'show' (for discoverability, since our @show@ returns @Text@).
showT :: Show a => a -> Text
showT = show

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

-- | Is the first Text a prefix of the second?
isPrefixOf :: Text -> Text -> Bool
isPrefixOf = T.isPrefixOf
{-# INLINE isPrefixOf #-}

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

-- | Monomorphic even/odd for Int.
-- The polymorphic Prelude versions go through the Integral typeclass
-- dictionary which contains error branches that the JIT evaluates eagerly.
even :: Int -> Bool
even n = n `rem` 2 == 0
{-# INLINE even #-}

odd :: Int -> Bool
odd n = n `rem` 2 /= 0
{-# INLINE odd #-}

-- | Monomorphic round :: Double -> Int.
-- The polymorphic Prelude round goes through RealFrac/properFraction which
-- pulls in dictionary error branches. This uses truncate (which works) and
-- manual fractional-part checking for banker's rounding.
round :: Double -> Int
round d =
  let n = truncate d :: Int
      f = d - fromIntegral n  -- fractional part
      af = if f < 0.0 then negate f else f
  in if af < 0.5 then n
     else if af > 0.5 then (if f > 0.0 then n + 1 else n - 1)
     else if even n then n  -- banker's rounding: round to even on .5
          else (if f > 0.0 then n + 1 else n - 1)
{-# INLINE round #-}

-- | Zip three lists with a function.
zipWith3 :: (a -> b -> c -> d) -> [a] -> [b] -> [c] -> [d]
zipWith3 f (a:as) (b:bs) (c:cs) = f a b c : zipWith3 f as bs cs
zipWith3 _ _ _ _ = []
{-# INLINE zipWith3 #-}

-- | Zip four lists with a function.
zipWith4 :: (a -> b -> c -> d -> e) -> [a] -> [b] -> [c] -> [d] -> [e]
zipWith4 f (a:as) (b:bs) (c:cs) (d:ds) = f a b c d : zipWith4 f as bs cs ds
zipWith4 _ _ _ _ _ = []
{-# INLINE zipWith4 #-}

-- | Apply a binary function with arguments from a projection.
on :: (b -> b -> c) -> (a -> b) -> a -> a -> c
on f g x y = f (g x) (g y)
{-# INLINE on #-}

-- | Build a comparison from a projection.
comparing :: Ord b => (a -> b) -> a -> a -> Ordering
comparing f x y = compare (f x) (f y)
{-# INLINE comparing #-}

-- ---------------------------------------------------------------------------
-- Text-to-number parsing (avoids Read typeclass which crashes the JIT)
-- ---------------------------------------------------------------------------

-- | Parse an integer from Text, returning Nothing on failure.
parseIntM :: Text -> Maybe Int
parseIntM t = case T.uncons t of
  Nothing -> Nothing
  Just ('-', rest) -> negate <$> parseNat rest
  Just ('+', rest) -> parseNat rest
  Just _           -> parseNat t
  where
    parseNat :: Text -> Maybe Int
    parseNat s
      | T.null s          = Nothing
      | T.all isDigitC s  = Just (T.foldl' (\acc c -> acc * 10 + (ord c - ord '0')) 0 s)
      | otherwise         = Nothing
    isDigitC :: Char -> Bool
    isDigitC c = c >= '0' && c <= '9'

-- | Parse an integer from Text, calling error on failure.
parseInt :: Text -> Int
parseInt t = fromMaybe (error ("parseInt: not a number: " <> T.unpack t)) (parseIntM t)

-- | Parse a Double from Text, returning Nothing on failure.
-- Handles optional sign, integer part, optional decimal part.
parseDoubleM :: Text -> Maybe Double
parseDoubleM t = case T.uncons t of
  Nothing -> Nothing
  Just ('-', rest) -> negate <$> parsePos rest
  Just ('+', rest) -> parsePos rest
  Just _           -> parsePos t
  where
    parsePos :: Text -> Maybe Double
    parsePos s = case T.break (== '.') s of
      (intPart, rest)
        | T.null intPart -> Nothing
        | not (T.all isDigitC intPart) -> Nothing
        | T.null rest ->
            Just (fromIntegral (parseDigits intPart))
        | otherwise -> case T.uncons rest of
            Just ('.', fracPart)
              | T.null fracPart -> Just (fromIntegral (parseDigits intPart))
              | T.all isDigitC fracPart ->
                  let whole = fromIntegral (parseDigits intPart) :: Double
                      frac  = fromIntegral (parseDigits fracPart) :: Double
                      denom = fromIntegral (pow10 (T.length fracPart)) :: Double
                  in  Just (whole + frac / denom)
              | otherwise -> Nothing
            _ -> Nothing
    parseDigits :: Text -> Int
    parseDigits = T.foldl' (\acc c -> acc * 10 + (ord c - ord '0')) 0
    pow10 :: Int -> Int
    pow10 0 = 1
    pow10 !n = 10 * pow10 (n - 1)
    isDigitC :: Char -> Bool
    isDigitC c = c >= '0' && c <= '9'

-- | Parse a Double from Text, calling error on failure.
parseDouble :: Text -> Double
parseDouble t = fromMaybe (error ("parseDouble: not a number: " <> T.unpack t)) (parseDoubleM t)

-- ---------------------------------------------------------------------------
-- JSON Value helpers
-- ---------------------------------------------------------------------------

-- | Safe key lookup: @v ?. "name"@ returns @Just val@ or @Nothing@.
(?.) :: Value -> Text -> Maybe Value
Object o ?. k = Map.lookup (fromText k) o
_        ?. _ = Nothing
infixl 9 ?.
{-# INLINE (?.) #-}

-- | Lookup a key in a Value, returning Nothing if not an Object or key missing.
lookupKey :: Text -> Value -> Maybe Value
lookupKey k (Object o) = Map.lookup (fromText k) o
lookupKey _ _          = Nothing
{-# INLINE lookupKey #-}

-- | Extract Text from a String Value, or Nothing.
asText :: Value -> Maybe Text
asText (String t) = Just t
asText _          = Nothing
{-# INLINE asText #-}

-- | Extract Int from a Number Value (truncates), or Nothing.
asInt :: Value -> Maybe Int
asInt (Number d) = Just (truncate d)
asInt _          = Nothing
{-# INLINE asInt #-}

-- | Extract Double from a Number Value, or Nothing.
asDouble :: Value -> Maybe Double
asDouble (Number d) = Just d
asDouble _          = Nothing
{-# INLINE asDouble #-}

-- | Extract Bool from a Bool Value, or Nothing.
asBool :: Value -> Maybe Bool
asBool (Bool b) = Just b
asBool _        = Nothing
{-# INLINE asBool #-}

-- | Extract the array from an Array Value, or Nothing.
asArray :: Value -> Maybe [Value]
asArray (Array a) = Just a
asArray _         = Nothing
{-# INLINE asArray #-}

-- | Extract the object from an Object Value, or Nothing.
asObject :: Value -> Maybe (Map.Map Key Value)
asObject (Object o) = Just o
asObject _          = Nothing
{-# INLINE asObject #-}

-- ---------------------------------------------------------------------------
-- Char predicates (monomorphic, range-based — avoids Data.Char dictionaries)
-- ---------------------------------------------------------------------------

-- | Is the character a decimal digit (0-9)?
isDigit :: Char -> Bool
isDigit c = c >= '0' && c <= '9'
{-# INLINE isDigit #-}

-- | Is the character an ASCII letter?
isAlpha :: Char -> Bool
isAlpha c = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
{-# INLINE isAlpha #-}

-- | Is the character an ASCII letter or digit?
isAlphaNum :: Char -> Bool
isAlphaNum c = isAlpha c || isDigit c
{-# INLINE isAlphaNum #-}

-- | Is the character ASCII whitespace?
isSpace :: Char -> Bool
isSpace c = c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v'
{-# INLINE isSpace #-}

-- | Is the character an ASCII uppercase letter?
isUpper :: Char -> Bool
isUpper c = c >= 'A' && c <= 'Z'
{-# INLINE isUpper #-}

-- | Is the character an ASCII lowercase letter?
isLower :: Char -> Bool
isLower c = c >= 'a' && c <= 'z'
{-# INLINE isLower #-}

-- | Convert a digit character to its numeric value.
-- Returns -1 for non-digit characters (avoids pulling in error dictionaries).
digitToInt :: Char -> Int
digitToInt c
  | c >= '0' && c <= '9' = ord c - ord '0'
  | c >= 'a' && c <= 'f' = ord c - ord 'a' + 10
  | c >= 'A' && c <= 'F' = ord c - ord 'A' + 10
  | otherwise             = -1
{-# INLINE digitToInt #-}

-- | Convert an ASCII character to lowercase.
toLowerChar :: Char -> Char
toLowerChar c
  | c >= 'A' && c <= 'Z' = chr (ord c + 32)
  | otherwise             = c
{-# INLINE toLowerChar #-}

-- | Convert an ASCII character to uppercase.
toUpperChar :: Char -> Char
toUpperChar c
  | c >= 'a' && c <= 'z' = chr (ord c - 32)
  | otherwise             = c
{-# INLINE toUpperChar #-}

-- ---------------------------------------------------------------------------
-- Monomorphic numeric helpers (avoids Num/Ord dictionary issues)
-- ---------------------------------------------------------------------------

-- | Monomorphic absolute value for Int.
abs' :: Int -> Int
abs' n = if n < 0 then negate n else n
{-# INLINE abs' #-}

-- | Monomorphic signum for Int.
signum' :: Int -> Int
signum' n
  | n < 0     = -1
  | n == 0    = 0
  | otherwise = 1
{-# INLINE signum' #-}

-- | Monomorphic min for Int.
min' :: Int -> Int -> Int
min' a b = if a <= b then a else b
{-# INLINE min' #-}

-- | Monomorphic max for Int.
max' :: Int -> Int -> Int
max' a b = if a >= b then a else b
{-# INLINE max' #-}

-- ---------------------------------------------------------------------------
-- Indexed list operations (safe alternatives to [0..])
-- The JIT evaluates data constructor fields eagerly, so infinite lists
-- crash with SIGSEGV.  These helpers avoid infinite lists entirely.
-- ---------------------------------------------------------------------------

-- | Pair each element with its 0-based index.
-- @zipWithIndex ["a","b","c"] == [(0,"a"),(1,"b"),(2,"c")]@
zipWithIndex :: [a] -> [(Int, a)]
zipWithIndex = go 0
  where
    go _ []     = []
    go !i (x:xs) = (i, x) : go (i + 1) xs
{-# INLINE zipWithIndex #-}

-- | Map with 0-based index.
-- @imap (\i x -> (i, x)) ["a","b"] == [(0,"a"),(1,"b")]@
imap :: (Int -> a -> b) -> [a] -> [b]
imap f = go 0
  where
    go _ []     = []
    go !i (x:xs) = f i x : go (i + 1) xs
{-# INLINE imap #-}

-- | Monomorphic enumFromTo for Int. Finite range, no infinite lists.
-- @enumFromTo 0 4 == [0,1,2,3,4]@
enumFromTo :: Int -> Int -> [Int]
enumFromTo lo hi
  | lo > hi   = []
  | otherwise = lo : enumFromTo (lo + 1) hi
{-# INLINE enumFromTo #-}

-- ---------------------------------------------------------------------------
-- Additional list combinators (P2)
-- ---------------------------------------------------------------------------

-- | Index of the first element equal to the target.
elemIndex :: Eq a => a -> [a] -> Maybe Int
elemIndex x = go 0
  where
    go _ []     = Nothing
    go !i (y:ys)
      | x == y    = Just i
      | otherwise = go (i + 1) ys
{-# INLINABLE elemIndex #-}

-- | Index of the first element satisfying the predicate.
findIndex :: (a -> Bool) -> [a] -> Maybe Int
findIndex p = go 0
  where
    go _ []     = Nothing
    go !i (x:xs)
      | p x       = Just i
      | otherwise = go (i + 1) xs
{-# INLINE findIndex #-}

-- | Zip three lists.
zip3 :: [a] -> [b] -> [c] -> [(a, b, c)]
zip3 (a:as) (b:bs) (c:cs) = (a, b, c) : zip3 as bs cs
zip3 _ _ _ = []
{-# INLINE zip3 #-}

-- | Unzip a list of triples.
unzip3 :: [(a, b, c)] -> ([a], [b], [c])
unzip3 [] = ([], [], [])
unzip3 ((a,b,c):rest) = let (as, bs, cs) = unzip3 rest in (a:as, b:bs, c:cs)
{-# INLINE unzip3 #-}

