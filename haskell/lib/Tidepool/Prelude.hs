{-# LANGUAGE BangPatterns, NoImplicitPrelude, FlexibleInstances, DuplicateRecordFields, DataKinds, TypeOperators #-}
-- | Self-contained prelude for Tidepool user code.
--
-- With NoImplicitPrelude in the MCP template, this is the single import.
-- Nothing from base Prelude is re-exported — every function is either
-- defined here or explicitly re-exported from a known-safe base module.
module Tidepool.Prelude
  ( -- * Types (re-exported from base)
    Int, Integer, Word, Char, Bool(..), Double, Float
  , String, Ordering(..), Maybe(..), Either(..)
  , Generic
    -- NonEmpty is intentionally NOT re-exported: `maximumBy (compare `on` snd)`
    -- and friends cover the argmax case without the NE machinery. Users who
    -- genuinely need it can `import Data.List.NonEmpty as NE` explicitly.
    -- * Bifunctor first/second (polymorphic; bimap comes via Control.Lens below)
  , first, second
    -- * Text type (re-exported from Data.Text)
  , Text
  , Pack(..), unpack
  , Render(render)
    -- * [fmt|...|] format-spec runtime helpers
  , FSign(..), FAlign(..)
  , fmtInt, fmtFrac, fmtStr, fmtChar, fmtSigned, fmtPlain
  , toUpper, toLower
  , strip
  , splitOn
  , replace
  , isSuffixOf, isInfixOf
    -- * Text versions of words/lines
  , words, lines, unwords, unlines
    -- * Typeclasses (re-exported from base)
  , Eq(..), Ord(..), Num(..), Integral(..), Real, Fractional(..), Floating(..), Show
  , Bounded(minBound, maxBound)
  , Semigroup(..), Monoid(..)
    -- * Monoid / Semigroup aggregation newtypes
  , Sum(..), Product(..), Any(..), All(..), First(..), Last(..), Endo(..)
  , Max(..), Min(..), Arg(..)
  , fromIntegral, realToFrac, truncate, ceiling, floor, round
  , (^), (^^), atan2
  , Functor(..), Applicative(..), Monad(..)
  , (<$>)
    -- * show (Text-returning shadow)
  , show, showT
  , showDouble
    -- * read (String-based, works on the JIT since native bignum — see
    -- gotcha_registry stale_doc_read_now_works; parseInt/parseDouble are the
    -- Text-first equivalents)
  , Read, read
    -- * Basic functions (re-exported from base)
  , id, const, flip, (.), ($), ($!)
  , not, (&&), (||), otherwise, seq
  , fst, snd, curry, uncurry
  , subtract
  , error, undefined
    -- * List operations
  , map, filter, foldl, foldl', foldr, foldMap
  , null
  , take, drop, zip, zipWith, unzip
  , lookup, elem, notElem
  , any, all, and, or
  , sum, product, minimum, maximum
  , concat, iterate, repeat, cycle
  , scanl, scanr, scanl1, scanr1
    -- * Self-contained list operations
  , reverse
  , splitAt
  , span
  , break
  , nub
  , nubBy
  , sort
  , sortBy
  , maximumBy, minimumBy
  , concatMap, concatMapM
  , append
  , (++)
  , dropWhile
  , length
  , replicate
  , isPrefixOf
  , intersperse
    -- * Text intercalate (shadows list version)
  , intercalate
  , joinText
  , tReverse
    -- * Text takeWhile/dropWhile (shadows T.takeWhile/T.dropWhile to avoid PAP bug)
  , takeWhileT
  , dropWhileT
    -- * Polymorphic typeclasses (work on both Text and [a])
  , Len(..), Null(..), Slice(..)
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
    -- * Additional list combinators (P3)
  , inits
  , group
  , scanl'
  , listIntercalate
    -- * Function combinators
  , on, (>>>), (<<<)
  , comparing
  , until
    -- * Monadic combinators
  , mapM, mapM_, sequence, sequence_, sequenceA
  , traverse_, for_, for
  , when, unless, void, join, guard
  , forM, forM_
  , (=<<), (>=>), (<=<)  -- (<&>) comes via module Control.Lens
  , (<|>)                -- Alternative (Maybe/[]); Control.Lens omits it
  , foldM, foldM_
  , filterM, replicateM, zipWithM
    -- * Maybe/Either utilities
  , maybe, fromMaybe, isJust, isNothing, catMaybes, mapMaybe, listToMaybe, maybeToList
  , either
    -- * Safe list heads — total forms; prefer these.
  , headMay, lastMay, initMay, tailMay, atMay, maximumMay, minimumMay
    -- * Partial shadows: EXPORTED, but each has an Unsatisfiable type, so
    -- referencing one is a compile error that names the total form to use.
    -- The base partials stay reachable qualified (L.head, Data.Maybe.fromJust)
    -- for the rare deliberate use.
  , head, tail, last, init, (!!), foldr1, foldl1, fromJust
  , readMaybe
    -- * Railway-oriented error helpers (errors package)
  , note, hush
    -- * Effectful filter-map (witherable package)
  , wither, filterA, ordNub
    -- * Numeric utilities
  , even, odd
    -- * Text-to-number parsing
  , parseIntM, parseInt, parseDoubleM, parseDouble
    -- * Char predicates & conversions
  , ord, chr, fromEnum, succ, pred, toEnum
  , isDigit, isAlpha, isAlphaNum, isSpace, isUpper, isLower
  , digitToInt, toLowerChar, toUpperChar
    -- * Indexed list operations (safe alternatives to [0..])
  , zipWithIndex, imap, enumFromTo
    -- * Kleisli profunctor squad (monadic Arrow-style plumbing)
  , (&&&), (***), (|||), firstK, secondK
    -- * Additional list combinators (P2)
  , elemIndex, findIndex
  , zip3, unzip3
    -- * Map/Set types
  , Map, Set
    -- * File paths (Tidepool.FilePath — System.FilePath over Text)
  , FilePath, pathSeparator, (</>), joinPath, splitFileName, splitDirectories, normalise
  , takeFileName, takeBaseName, takeDirectory
  , (<.>), (-<.>), takeExtension, takeExtensions, dropExtension, dropExtensions
  , addExtension, replaceExtension, splitExtension, hasExtension, isExtensionOf
  , isAbsolute, isRelative
    -- * JSON (Tidepool.Aeson — vendored, construction-only)
  , Value(..), Scientific, scientific, coefficient, base10Exponent
  , fromFloatDigits, toRealFloat
  , Key, object, (.=), toJSON
  , ToJSON
  , FromJSON(..), Result(..), fromJSON, resultToEither, eitherDecode, decode
  , (.:), (.:?), (.!=), withObject, withText, withArray, withBool, withDouble
    -- * JSON lenses (Tidepool.Aeson.Lens + Control.Lens)
  , key, nth, _String, _Number, _Bool, _Array, _Object, _Int, _Integer, _Double
  , members, values, _Null
    -- * ALL of Control.Lens, re-exported wholesale (indexed optics ^@.. / itoListOf,
    -- partsOf, _head/_last/_init/_tail, prism/iso/lens builders, the Bifunctor
    -- first/second/bimap, etc.). Only two names keep the Tidepool version (hidden
    -- from the import): `imap` (Prelude's list-index map) and `(.=)` (Aeson's
    -- object-pair operator).
  , module Control.Lens
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
    -- * Map operations (extended — non-conflicting additions)
    -- Note: Map.filter/partition/foldr/foldl' share names with list
    -- functions already exported, so call them as Map.filter etc. (via
    -- the preamble's auto-imported `Map.` qualifier).
  , Map.toAscList, Map.fromListWith
  , alter
  , unionsWith
  , (!?)
    -- * Set operations
    -- Set.* functions all share names with Map.* or list functions
    -- already in scope. Use the preamble's auto-imported `Set.` qualifier
    -- (Set.fromList, Set.member, Set.insert, …) — it is always available.
    -- Local helpers for patterns that benefit from a safe implementation:
  , setUnions
    -- * Map helpers (local impls — unqualified, unlike Map.* re-exports above)
  , insertWith
    -- * Prelude workhorses
  , sortOn, Down(..), swap
  , partitionEithers, rights, lefts, fromLeft, fromRight
    -- * Shared record vocabulary (Tidepool.Records)
  , Proc(..), ok, Hit(..)
  , FileMeta(..), UpdateOutcome(..), UpdateOneOutcome(..), WriteOutcome(..)
  , UpdateAllOutcome(..), InsertAfterOutcome(..)
  , Commit(..), StatusEntry(..), FileDelta(..)
    -- * Text padding, chunking, and prefix utilities
    -- (Text chunking is `T.chunksOf`; the unqualified `chunksOf` is the list
    -- chunker from `.tidepool/lib/Schemes.hs` — do not shadow it here.)
  , justifyLeft, justifyRight, center
  , textReplicate
  , commonPrefixes
    -- * UTC time (Tidepool.Data.Time)
  , UTCTime(..), formatISO8601, parseISO8601, toGregorian, formatDay, daysFromCivil
  , diffUTCTime, addUTCTime, epochMillis
    -- Deliberately absent from the unqualified shadow — each canonical
    -- Prelude/Data.List name below is reached through a qualifier instead
    -- (tidepool://capabilities is the served index; the reasons live in
    -- resources.rs `QUALIFIED_NAMES`, which the compile error-hint path reads):
    --   list combinators → Data.List (L.): subsequences, permutations, delete,
    --     insert, union, intersect, stripPrefix, mapAccumR, foldl1',
    --     isSubsequenceOf, genericTake/genericDrop
    --   rendering/parsing: showsPrec/shows/showString → `show :: a -> Text`;
    --     reads/readsPrec → `read` + parseInt/parseIntM/parseDouble/parseDoubleM
    --   IO console/stdin: print/getLine/interact → the Console effect + `input` lane
    --   numeric → base (P.): gcd, lcm, properFraction
    --   ranges: enumFrom/enumFromThen → `enumFromTo lo hi` ([lo..hi] desugars to it)
  ) where

import GHC.Generics (Generic)
import Prelude
  ( Int, Integer, Word, Char, Bool(..), Double, Float
  , String, Ordering(..), Maybe(..), Either(..)
  , Eq(..), Ord(..), Num(..), Integral(..), Real, Fractional(..), Floating(..), Show
  , Bounded(minBound, maxBound)
  , Read, read
  , Semigroup(..), Monoid(..)
  , fromIntegral, realToFrac, truncate, ceiling, floor, round, even, odd
  , (^), (^^), atan2
  , Functor(..), Applicative(..), Monad(..)
  , (<$>)
  , id, const, flip, (.), ($), ($!)
  , not, (&&), (||), otherwise, seq
  , fst, snd, curry, uncurry
  , error, undefined
  , maybe, either
  , map, foldl, foldr, foldMap
  , take, drop, zip, zipWith, unzip
  , lookup, elem, notElem
  , any, all, and, or
  , sum, product, minimum, maximum
  , concat, iterate, repeat, cycle
  , scanl, scanr, scanl1, scanr1
  , negate, quot, rem, subtract
  , compare
  , fromEnum, succ, pred, toEnum
  , mapM, mapM_, sequence, sequence_, sequenceA
  )
import Data.Foldable (traverse_, for_)
import Data.Traversable (for)
import qualified Prelude as P (show, drop, length, null, dropWhile)
import Data.Text (Text)
-- Vendored drop-in for Data.Text: re-exports all of Data.Text but overrides the
-- (Char -> Bool)-taking functions (takeWhile/dropWhile/span/break/filter/all/…)
-- with HOME-module bodies, so wrapping them here (or in a lib verb) with an
-- operator-section predicate is correct on the JIT. See Tidepool.Data.Text and
-- the text-vendor-mechanism-proven memory.
import qualified Tidepool.Data.Text as T
import Tidepool.Data.Text (Pack(..), pack)
import Tidepool.FilePath
import Data.Char (ord, chr)
import qualified Data.Char as C
import Data.Maybe (fromMaybe, isJust, isNothing, catMaybes, mapMaybe, listToMaybe, maybeToList)
import GHC.TypeError (Unsatisfiable, unsatisfiable, ErrorMessage(..))
import Data.List (foldl', find, partition, groupBy, takeWhile, tails, unfoldr, mapAccumL, transpose, genericLength, sort, sortBy, sortOn, maximumBy, minimumBy, inits, group, scanl')
-- Bifunctor first/second (polymorphic — tuples AND Either). Control.Lens
-- re-exports `bimap` but NOT first/second, so import those two from the library.
import Data.Bifunctor (first, second)
import Data.Monoid (Sum(..), Product(..), Any(..), All(..), First(..), Last(..), Endo(..), appEndo)
import Data.Semigroup (Max(..), Min(..), Arg(..))
import Data.Ord (Down(..))
import Data.Tuple (swap)
import Data.Either (partitionEithers, rights, lefts, fromLeft, fromRight)
import Data.Map.Strict (Map)
import Data.Set (Set)
import Control.Monad
  ( when, unless, void, join, guard
  , forM, forM_
  , (=<<), (>=>), (<=<)
  , foldM, foldM_
  )
import Tidepool.Records (Proc(..), ok, Hit(..), FileMeta(..), UpdateOutcome(..), UpdateOneOutcome(..), WriteOutcome(..), UpdateAllOutcome(..), InsertAfterOutcome(..), Commit(..), StatusEntry(..), FileDelta(..))
import Tidepool.Data.Time (UTCTime(..), formatISO8601, parseISO8601, toGregorian, formatDay, daysFromCivil, diffUTCTime, addUTCTime, epochMillis)
import Tidepool.Render (Render(..))
import Tidepool.QQ.Fmt.Runtime
  (FSign(..), FAlign(..), fmtInt, fmtFrac, fmtStr, fmtChar, fmtSigned, fmtPlain)
import Tidepool.Aeson (Value(..), Scientific, scientific, coefficient, base10Exponent, fromFloatDigits, toRealFloat, Key, object, (.=), toJSON, ToJSON, fromText, eitherDecode, decode, FromJSON(..), Result(..), fromJSON, resultToEither, (.:), (.:?), (.!=), withObject, withText, withArray, withBool, withDouble)
import Tidepool.Aeson.Scientific (toBoundedInteger)
import Tidepool.Aeson.Lens (key, nth, _String, _Number, _Bool, _Array, _Object, _Int, _Integer, _Double, members, values, _Null)
-- Wholesale Control.Lens, hiding only the two genuine clashes: `imap` (Prelude's
-- list-index map, defined below) and `(.=)` (Aeson's object-pair operator, above).
-- Hidden from the wholesale import because they clash with names we keep:
--   imap — Prelude's own list-index map (above);
--   (.=) — Aeson's object-pair operator;
--   (??) — removed heuristic LLM operator (kept absent, not aliased to lens's
--          flipped-apply);
--   para — removed (was the `Schemes.para` list paramorphism, since cut); lens's
--          `para` is the niche Plated one, kept hidden so `para` stays unbound;
--   (<.>) — System.FilePath's add-extension operator (Tidepool.FilePath, above);
--          lens's `(<.>)` is the niche indexed-optic composition.
--   rewrite — the structural ast-grep verb `Flow.rewrite` (Library); lens's
--             `rewrite` is the niche Plated bottom-up traversal, never wanted in eval.
import Control.Lens hiding (imap, (.=), (??), para, (<.>), rewrite)
import Control.Applicative ((<|>))  -- Alternative (<|>) — Control.Lens omits it
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
-- Point-free composition (Control.Category). `(&)`/`bimap` already arrive via
-- the wholesale Control.Lens re-export; these two do not.
import Control.Category ((>>>), (<<<))
-- Safe alternatives to the partial list heads (safe package). The unsafe
-- `head`/`tail`/`last`/`init`/`(!!)`/`foldr1`/`foldl1`/`fromJust` are deliberately
-- NOT re-exported — reach for these instead (each returns `Maybe`).
import Safe (headMay, lastMay, initMay, tailMay, atMay, maximumMay, minimumMay)
-- Railway-oriented error helpers (errors package): `note` tags a `Nothing` into
-- a `Left e`; `hush` forgets a `Left` back to `Nothing`. Compose with the
-- `Either`-returning verbs (#335) and `liftEither`/`partitionEithers`.
import Control.Error.Util (note, hush)
-- Filter-map fused inside an effect/Applicative (witherable package): `wither`
-- is `mapMaybe` with effects, `filterA` is `filter` with effects; `ordNub` is
-- the O(n log n) nub.
import Witherable (wither, filterA, ordNub)
-- Total parse (Text.Read). Text-first numeric parsing stays in parseInt/parseDouble.
import Text.Read (readMaybe)

-- Permanent binding-level interception in Translate.hs.
-- GHC's floatToDigits/Integer pipeline is fundamentally incompatible with
-- the JIT, so showDouble is always intercepted and emitted as ShowDoubleAddr.
-- The body is a fallback that should never run.
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

-- | Polymorphic @pack@ (identity on 'Text', pack on 'String') now lives in
-- 'Tidepool.Data.Text' so that the qualified @T.pack@ and this unqualified
-- @pack@ are the SAME function — @T.pack (show x)@ is no longer a trap.
-- Imported + re-exported below; see 'Tidepool.Data.Text.Pack'.

unpack :: Text -> String
unpack = T.unpack

toUpper :: Text -> Text
toUpper = T.toUpper

toLower :: Text -> Text
toLower = T.toLower

strip :: Text -> Text
strip = T.strip

-- Delegates to T.splitOn, which compiles and runs cleanly under today's JIT.
-- HISTORY (2026-06-11): this was a "pure reimplementation" that round-tripped
-- the ENTIRE text through T.unpack and char-matched over [Char] — measured
-- ~83x slower than T.splitOn at 2KB and super-linear beyond (333ms vs 4ms;
-- a 70KB file-surgery eval timed out at 120s). The String detour predates the
-- specialization/lazy-closure/GC fixes that made the real text-package path
-- viable. Empty separator keeps the singleton-explosion semantics (T.splitOn
-- errors on "").
splitOn :: Text -> Text -> [Text]
splitOn sep t
  | T.null sep = map (\c -> T.pack [c]) (T.unpack t)
  | otherwise  = T.splitOn sep t

replace :: Text -> Text -> Text -> Text
replace = T.replace

isSuffixOf :: Text -> Text -> Bool
isSuffixOf = T.isSuffixOf

isInfixOf :: Text -> Text -> Bool
isInfixOf = T.isInfixOf

-- Delegates to T.words. HISTORY (2026-06-11): was a "pure reimplementation"
-- round-tripping through [Char]; probe-verified T.words compiles and runs
-- cleanly under today's JIT in both saturated AND higher-order position, so the
-- String detour only cost performance. Same retirement as splitOn (3fb10e5).
-- (Contrast takeWhileT/dropWhileT below: the analogous delegation to Data.Text
-- was MEASURED BROKEN, so their String detour stays load-bearing.)
words :: Text -> [Text]
words = T.words
{-# INLINE words #-}

-- Delegates to T.lines. Same retirement as words above; edge semantics
-- verified equivalent (trailing newline, empty input, bare "\n").
lines :: Text -> [Text]
lines = T.lines
{-# INLINE lines #-}

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
    go m ys | m <= 0 = ([], ys)
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


-- | Map a function over a list and concatenate results.
concatMap :: (a -> [b]) -> [a] -> [b]
concatMap _ [] = []
concatMap f (x:xs) = go (f x)
  where
    go []     = concatMap f xs
    go (y:ys) = y : go ys
{-# INLINE concatMap #-}

-- | Monadic concatMap: map an effectful function over a list and concatenate results.
-- @concatMapM f xs = fmap concat (mapM f xs)@
concatMapM :: Monad m => (a -> m [b]) -> [a] -> m [b]
concatMapM f xs = fmap concat (mapM f xs)
{-# INLINE concatMapM #-}

-- | Monadic filter: keep elements for which the effectful predicate returns True.
-- @filterM (\\f -> isInfixOf "unsafe" \<$\> fsRead f) files@
filterM :: Monad m => (a -> m Bool) -> [a] -> m [a]
filterM _ []     = pure []
filterM p (x:xs) = do
  keep <- p x
  rest <- filterM p xs
  pure (if keep then x : rest else rest)

-- | Repeat an effect N times, collecting results.
-- @replicateM 3 (ask "next?")@
replicateM :: Monad m => Int -> m a -> m [a]
replicateM n act = go n
  where
    go i | i <= 0    = pure []
         | otherwise = do { x <- act; xs <- go (i - 1); pure (x : xs) }

-- | Zip two lists with an effectful function.
-- @zipWithM (\\a b -> llm schema (a \<\> b)) prompts contexts@
zipWithM :: Monad m => (a -> b -> m c) -> [a] -> [b] -> m [c]
zipWithM f (a:as) (b:bs) = do { c <- f a b; cs <- zipWithM f as bs; pure (c : cs) }
zipWithM _ _      _      = pure []

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
    go m | m <= 0    = []
         | otherwise = x : go (m - 1)
{-# INLINE replicate #-}

-- | Join a list of Texts with a separator. Shadows list intercalate.
-- For list intercalate, use @import qualified Data.List as L@ then @L.intercalate@.
intercalate :: Text -> [Text] -> Text
intercalate = T.intercalate
{-# INLINE intercalate #-}

-- | Alias for 'intercalate' (for discoverability).
joinText :: Text -> [Text] -> Text
joinText = T.intercalate
{-# INLINE joinText #-}

-- | Reverse a Text.
tReverse :: Text -> Text
tReverse = T.reverse
{-# INLINE tReverse #-}

-- | Text takeWhile: take the longest prefix of characters satisfying a predicate.
-- Thin alias for the vendored @T.takeWhile@ (@Tidepool.Data.Text@), kept for
-- source compatibility.
--
-- RETIRED (2026-06-20): the old @T.pack . go . T.unpack@ String-detour body was a
-- LOAD-BEARING workaround for the cross-module-wrapper + operator-section
-- corruption of EXTERNAL @Data.Text.takeWhile@ (gotcha-audit #14). With @T@ now
-- repointed to the vendored home-module @Tidepool.Data.Text@, @T.takeWhile@ is a
-- HOME body — proven correct under exactly this wrapped-section path (the probe
-- in text-vendor-mechanism-proven; guard @repro_takewhilet_alias_pap.rs@). So the
-- delegation that was MEASURED BROKEN against external text is now correct, and
-- the String detour (slow, allocating) is gone.
takeWhileT :: (Char -> Bool) -> Text -> Text
takeWhileT = T.takeWhile
{-# INLINE takeWhileT #-}

-- | Text dropWhile: drop the longest prefix of characters satisfying a predicate.
-- Thin alias for the vendored @T.dropWhile@; see @takeWhileT@ above. RETIRED
-- (2026-06-20) from the String-detour shadow now that @T@ is the vendored
-- home-module Data.Text.
dropWhileT :: (Char -> Bool) -> Text -> Text
dropWhileT = T.dropWhile
{-# INLINE dropWhileT #-}

-- ---------------------------------------------------------------------------
-- Polymorphic typeclasses (work on both Text and [a])
-- ---------------------------------------------------------------------------

-- | Length of a container. Works on both Text and lists.
class Len a where
  len :: a -> Int

instance Len Text where
  len = T.length
  {-# INLINE len #-}

instance Len [a] where
  len = go 0
    where
      go :: Int -> [a] -> Int
      go !acc []     = acc
      go !acc (_:xs) = go (acc + 1) xs
  {-# INLINE len #-}

-- | Emptiness check. Works on both Text and lists.
class Null a where
  isNull :: a -> Bool

instance Null Text where
  isNull = T.null
  {-# INLINE isNull #-}

instance Null [a] where
  isNull [] = True
  isNull _  = False
  {-# INLINE isNull #-}

-- | Take/drop prefix. Works on both Text and lists.
-- Named @stake@/@sdrop@ to avoid shadowing list @take@/@drop@.
class Slice a where
  stake :: Int -> a -> a
  sdrop :: Int -> a -> a

instance Slice Text where
  stake = T.take
  sdrop = T.drop
  {-# INLINE stake #-}
  {-# INLINE sdrop #-}

instance Slice [a] where
  stake n _      | n <= 0 = []
  stake _ []     = []
  stake n (x:xs) = x : stake (n-1) xs
  sdrop n xs     | n <= 0 = xs
  sdrop _ []     = []
  sdrop n (_:xs) = sdrop (n-1) xs
  {-# INLINE stake #-}
  {-# INLINE sdrop #-}

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

-- Partial-function shadows. These eight classic partials are EXPORTED, but
-- each carries an Unsatisfiable constraint: the definitions below compile
-- clean, while any CALLER's `head xs` is a compile error whose message names
-- the total replacement. Unsatisfiable (not a bare TypeError, which fires at
-- the definition) is what defers the error to the use site. The base partials
-- remain reachable qualified (L.head / Data.Maybe.fromJust) for deliberate use.
head :: Unsatisfiable ('Text "head is partial — use headMay :: [a] -> Maybe a." ':$$: 'Text "Deliberate partial use: L.head (qualified Data.List).") => [a] -> a
head = unsatisfiable

tail :: Unsatisfiable ('Text "tail is partial — use tailMay :: [a] -> Maybe [a]." ':$$: 'Text "Deliberate partial use: L.tail (qualified Data.List).") => [a] -> [a]
tail = unsatisfiable

last :: Unsatisfiable ('Text "last is partial — use lastMay :: [a] -> Maybe a." ':$$: 'Text "Deliberate partial use: L.last (qualified Data.List).") => [a] -> a
last = unsatisfiable

init :: Unsatisfiable ('Text "init is partial — use initMay :: [a] -> Maybe [a]." ':$$: 'Text "Deliberate partial use: L.init (qualified Data.List).") => [a] -> [a]
init = unsatisfiable

(!!) :: Unsatisfiable ('Text "(!!) is partial — use atMay xs i :: Maybe a." ':$$: 'Text "Deliberate partial use: (L.!!) (qualified Data.List).") => [a] -> Int -> a
(!!) = unsatisfiable

foldr1 :: Unsatisfiable ('Text "foldr1 is partial — seed the fold with foldr." ':$$: 'Text "Deliberate partial use: L.foldr1 (qualified Data.List).") => (a -> a -> a) -> [a] -> a
foldr1 = unsatisfiable

foldl1 :: Unsatisfiable ('Text "foldl1 is partial — seed the fold with foldl'." ':$$: 'Text "Deliberate partial use: L.foldl1 (qualified Data.List).") => (a -> a -> a) -> [a] -> a
foldl1 = unsatisfiable

fromJust :: Unsatisfiable ('Text "fromJust is partial — use fromMaybe def, maybe, or a Just pattern." ':$$: 'Text "Deliberate partial use: Data.Maybe.fromJust.") => Maybe a -> a
fromJust = unsatisfiable

-- #155: Monomorphic even/odd shadows removed — GHC specialization
-- (re-enabled) eliminates Integral dictionary passing at compile time.
-- Likewise `round` (was `Double -> Int`): it now comes polymorphically from
-- base, matching its already-polymorphic siblings `truncate`/`floor`/`ceiling`,
-- so `round`/`truncate`/`floor`/`ceiling` work on any `RealFrac` — including
-- `Scientific`, exactly (Integer math, no `Double` round-trip). The Double case
-- still lowers to the round primop (as `QQ.Fmt.Runtime` already relies on).
-- The same cleanup was finished for the last four `Int`-only shadows
-- (`abs'`/`signum'`/`min'`/`max'`): removed in favour of the polymorphic base
-- `abs`/`signum` (via `Num(..)`) and `max`/`min` (via `Ord(..)`), already
-- re-exported above.

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

-- | Iterate @f@ from @x@ until @p@ holds, returning the first value that
-- satisfies it. Tail recursion (unbounded on the JIT), same shape as 'iterate'.
until :: (a -> Bool) -> (a -> a) -> a -> a
until p f = go
  where
    go x = if p x then x else go (f x)
{-# INLINE until #-}

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

-- | Partial shadow (see the head/fromJust shadows above): exported but any
-- caller is a compile error naming the total form. Parsing can always fail, so
-- the total-returning `parseIntM` is the only sanctioned surface.
parseInt :: Unsatisfiable ('Text "parseInt is partial — use parseIntM :: Text -> Maybe Int." ':$$: 'Text "Then handle the Nothing (fromMaybe def, a case, or liftMaybe).") => Text -> Int
parseInt = unsatisfiable

-- | Parse a Double from Text, returning Nothing on failure. Handles optional
-- sign, integer part, optional decimal part, and an optional @e@\/@E@
-- exponent (so it round-trips 'showDouble', which emits scientific notation
-- for very small\/large magnitudes). Digits accumulate directly as a
-- 'Double' rather than an 'Int', so a digit run longer than ~19 characters
-- loses precision the way any Double parse would instead of silently
-- overflowing to garbage.
parseDoubleM :: Text -> Maybe Double
parseDoubleM t = case T.uncons t of
  Nothing -> Nothing
  Just ('-', rest) -> negate <$> parseSigned rest
  Just ('+', rest) -> parseSigned rest
  Just _           -> parseSigned t
  where
    parseSigned :: Text -> Maybe Double
    parseSigned s =
      let (mant, expPart) = T.break (\c -> c == 'e' || c == 'E') s
      in case parseMantissa mant of
           Nothing -> Nothing
           Just m  -> case T.uncons expPart of
             Nothing      -> Just m
             Just (_, er) -> scaleByPow10 m <$> parseExponent er

    parseMantissa :: Text -> Maybe Double
    parseMantissa s = case T.break (== '.') s of
      (intPart, rest)
        | T.null intPart -> Nothing
        | not (T.all isDigitC intPart) -> Nothing
        | T.null rest -> Just (digitsToDouble intPart)
        | otherwise -> case T.uncons rest of
            Just ('.', fracPart)
              | T.null fracPart -> Just (digitsToDouble intPart)
              | T.all isDigitC fracPart ->
                  Just (digitsToDouble intPart + digitsToDouble fracPart / pow10D (T.length fracPart))
              | otherwise -> Nothing
            _ -> Nothing

    parseExponent :: Text -> Maybe Int
    parseExponent e = case T.uncons e of
      Nothing       -> Nothing
      Just ('-', r) -> negate <$> parseNatInt r
      Just ('+', r) -> parseNatInt r
      Just _        -> parseNatInt e

    parseNatInt :: Text -> Maybe Int
    parseNatInt s
      | T.null s          = Nothing
      | T.all isDigitC s  = Just (T.foldl' (\acc c -> acc * 10 + (ord c - ord '0')) 0 s)
      | otherwise         = Nothing

    digitsToDouble :: Text -> Double
    digitsToDouble = T.foldl' (\acc c -> acc * 10 + fromIntegral (ord c - ord '0')) 0

    pow10D :: Int -> Double
    pow10D 0 = 1
    pow10D !n = 10 * pow10D (n - 1)

    -- ONE final multiplication/division against a precomputed power of ten
    -- (not N chained single-digit steps): 'pow10D' itself is exact for the
    -- exponents 'showDouble' ever emits (powers of ten up to 10^22 are
    -- exactly representable as 'Double'), and IEEE-754 division/
    -- multiplication is correctly-rounded, so this gives the same result a
    -- correctly-rounded decimal parser would -- chaining @/10@ or @*10@ once
    -- per exponent step instead compounds a rounding error at every step
    -- (measurably wrong for negative exponents, e.g. @parseDoubleM
    -- (showT (1.5e-10 :: Double))@ no longer round-tripped).
    scaleByPow10 :: Double -> Int -> Double
    scaleByPow10 x e
      | e >= 0    = x * pow10D e
      | otherwise = x / pow10D (negate e)

    isDigitC :: Char -> Bool
    isDigitC c = c >= '0' && c <= '9'

-- | Partial shadow (see `parseInt`): use the total `parseDoubleM`.
parseDouble :: Unsatisfiable ('Text "parseDouble is partial — use parseDoubleM :: Text -> Maybe Double." ':$$: 'Text "Then handle the Nothing (fromMaybe def, a case, or liftMaybe).") => Text -> Double
parseDouble = unsatisfiable

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

-- | Extract Int from a Number Value: @Just@ only for an in-range integral
-- number, @Nothing@ for a fractional or out-of-range one.
asInt :: Value -> Maybe Int
asInt (Number s) = toBoundedInteger s
asInt _          = Nothing
{-# INLINE asInt #-}

-- | Extract Double from a Number Value, or Nothing.
asDouble :: Value -> Maybe Double
asDouble (Number s) = Just (toRealFloat s)
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
-- Char predicates
-- ---------------------------------------------------------------------------

-- | Is the character a decimal digit (0-9)? ASCII-only, matching
-- @Data.Char.isDigit@ exactly (upstream is also ASCII-only here — Unicode
-- digits outside 0-9 are @isNumber@, not @isDigit@).
isDigit :: Char -> Bool
isDigit c = c >= '0' && c <= '9'
{-# INLINE isDigit #-}

-- | Is the character alphabetic, per full Unicode semantics (delegates to
-- @Data.Char.isAlpha@ — proven JIT-safe: the vendored 'Tidepool.Data.Text'
-- @T.words@ already runs the same Unicode @isSpace@ table on the JIT). An
-- ASCII-only range check here would silently diverge from @Data.Char@ on any
-- accented/non-Latin letter, contradicting the API-is-the-prompt rule.
isAlpha :: Char -> Bool
isAlpha = C.isAlpha
{-# INLINE isAlpha #-}

-- | Is the character alphabetic or a decimal digit, per full Unicode
-- semantics (delegates to @Data.Char.isAlphaNum@; see 'isAlpha').
isAlphaNum :: Char -> Bool
isAlphaNum = C.isAlphaNum
{-# INLINE isAlphaNum #-}

-- | Is the character whitespace, per full Unicode semantics (delegates to
-- @Data.Char.isSpace@; see 'isAlpha').
isSpace :: Char -> Bool
isSpace = C.isSpace
{-# INLINE isSpace #-}

-- | Is the character uppercase, per full Unicode semantics (delegates to
-- @Data.Char.isUpper@; see 'isAlpha').
isUpper :: Char -> Bool
isUpper = C.isUpper
{-# INLINE isUpper #-}

-- | Is the character lowercase, per full Unicode semantics (delegates to
-- @Data.Char.isLower@; see 'isAlpha').
isLower :: Char -> Bool
isLower = C.isLower
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
-- Kleisli profunctor squad (probe-verified under the JIT 2026-06-11):
-- Arrow-style plumbing for monadic pipelines, monomorphic in shape but
-- polymorphic over the monad (single-constraint Monad dictionaries are
-- JIT-safe — same class as mapM/foldM). Fixities match Control.Arrow.
--
-- >>> (\x -> pure (x + 1)) &&& (\x -> pure (x * 2)) $ 10   -- pure (11, 20)
-- >>> readFile *** fsMeta $ ("a.txt", "b.txt")              -- pair of effects
-- >>> (handleLeft ||| handleRight) someEither
-- ---------------------------------------------------------------------------

infixr 3 &&&
infixr 3 ***
infixr 2 |||

-- | Fan-out: run two Kleisli arrows on the same input, pair the results.
(&&&) :: Monad m => (a -> m b) -> (a -> m c) -> a -> m (b, c)
(f &&& g) x = do
  b <- f x
  c <- g x
  pure (b, c)

-- | Split: run one arrow on each component of a pair.
(***) :: Monad m => (a -> m b) -> (c -> m d) -> (a, c) -> m (b, d)
(f *** g) (a, c) = do
  b <- f a
  d <- g c
  pure (b, d)

-- | Fan-in: route an Either to one of two Kleisli arrows.
(|||) :: Monad m => (a -> m c) -> (b -> m c) -> Either a b -> m c
(f ||| _) (Left a)  = f a
(_ ||| g) (Right b) = g b

-- | Apply a Kleisli arrow to the first component of a pair.
firstK :: Monad m => (a -> m b) -> (a, c) -> m (b, c)
firstK f (a, c) = do
  b <- f a
  pure (b, c)

-- | Apply a Kleisli arrow to the second component of a pair.
secondK :: Monad m => (a -> m b) -> (c, a) -> m (c, b)
secondK f (c, a) = do
  b <- f a
  pure (c, b)

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

-- | Lazy filter (guarded corecursion).
filter :: (a -> Bool) -> [a] -> [a]
filter _ [] = []
filter p (x:xs)
  | p x = x : filter p xs
  | otherwise = filter p xs
{-# INLINE filter #-}

-- | Lazy nubBy (emits matches immediately, keeps seen-accumulator).
-- @eq@ is applied as @eq seenElem candidate@ (the already-kept element
-- first), matching base's argument order — matters when @eq@ is not a true
-- equivalence relation (e.g. a directional "subsumes" predicate).
nubBy :: (a -> a -> Bool) -> [a] -> [a]
nubBy eq = go []
  where
    go _ [] = []
    go seen (x:rest)
      | elemBy x seen = go seen rest
      | otherwise     = x : go (x : seen) rest
    elemBy _ []     = False
    elemBy x (y:ys)
      | eq y x    = True
      | otherwise = elemBy x ys
{-# INLINE nubBy #-}

-- | Tail-recursive nub (uses nubBy).
nub :: (Eq a) => [a] -> [a]
nub = nubBy (==)

-- ---------------------------------------------------------------------------
-- Map additional operations + local insertWith
-- ---------------------------------------------------------------------------

-- | @alter f k m@ — apply @f@ to the current value at @k@ (Nothing if absent),
-- then insert (Just v) or delete (Nothing) based on the result.
-- Implemented via Map.lookup/insert/delete to avoid GHC's complex internal
-- alter unfoldings (same safety rationale as local 'insertWith').
alter :: Ord k => (Maybe a -> Maybe a) -> k -> Map k a -> Map k a
alter f k m = case f (Map.lookup k m) of
  Nothing -> Map.delete k m
  Just !v -> Map.insert k v m
{-# INLINE alter #-}

-- | Fold a list of maps together with a combining function.
-- Uses foldl' to avoid stack overflow on large lists of maps.
-- Equivalent to @Data.Map.Strict.unionsWith@ but stack-safe.
unionsWith :: Ord k => (a -> a -> a) -> [Map k a] -> Map k a
unionsWith f = foldl' (Map.unionWith f) Map.empty
{-# INLINE unionsWith #-}

-- | Infix lookup: @m '!?' k == Map.lookup k m@.
(!?) :: Ord k => Map k a -> k -> Maybe a
(!?) = flip Map.lookup
infixl 9 !?
{-# INLINE (!?) #-}

-- ---------------------------------------------------------------------------
-- Set helpers
-- ---------------------------------------------------------------------------

-- | Union all sets in a list. Uses foldl' to stay stack-safe for large lists.
-- For small lists @Set.unions@ is equivalent.
setUnions :: Ord a => [Set a] -> Set a
setUnions = foldl' Set.union Set.empty
{-# INLINE setUnions #-}

-- ---------------------------------------------------------------------------
-- List combinators (P3)
-- ---------------------------------------------------------------------------

-- | Intercalate for lists (not Text). Named to avoid shadowing the Text
-- 'intercalate'; for Text use 'intercalate' (or 'joinText'), for lists use this.
-- @listIntercalate [0] [[1,2],[3,4]] == [1,2,0,3,4]@
listIntercalate :: [a] -> [[a]] -> [a]
listIntercalate sep = go
  where
    go []     = []
    go [x]    = x
    go (x:xs) = x ++ sep ++ go xs
{-# INLINE listIntercalate #-}

-- ---------------------------------------------------------------------------
-- Text padding, chunking, and prefix utilities
-- ---------------------------------------------------------------------------

-- | Left-justify text to width @w@, padding on the right with @c@.
-- @justifyLeft 10 ' ' "hello" == "hello     "@
justifyLeft :: Int -> Char -> Text -> Text
justifyLeft w c t =
  let !l = T.length t
  in if l >= w then t
     else t <> T.replicate (w - l) (T.singleton c)
{-# INLINE justifyLeft #-}

-- | Right-justify text to width @w@, padding on the left with @c@.
-- @justifyRight 10 ' ' "hello" == "     hello"@
justifyRight :: Int -> Char -> Text -> Text
justifyRight w c t =
  let !l = T.length t
  in if l >= w then t
     else T.replicate (w - l) (T.singleton c) <> t
{-# INLINE justifyRight #-}

-- | Center text to width @w@, padding with @c@ on both sides.
-- Left pad gets the extra character when @(w - length t)@ is odd.
-- @center 11 '-' "hello" == "---hello---"@
center :: Int -> Char -> Text -> Text
center w c t =
  let !l     = T.length t
  in if l >= w then t
     else let !total = w - l
              !rpad  = total `div` 2
              !lpad  = total - rpad
          in T.replicate lpad (T.singleton c) <> t <> T.replicate rpad (T.singleton c)
{-# INLINE center #-}

-- | Repeat a Text @n@ times: @textReplicate 3 "ab" == "ababab"@.
-- Thin alias for @T.replicate@ (distinct from list 'replicate').
textReplicate :: Int -> Text -> Text
textReplicate = T.replicate
{-# INLINE textReplicate #-}

-- | Find the longest common prefix of two Texts.
-- Returns @Just (prefix, rest1, rest2)@ or @Nothing@ if there is no common prefix.
-- @commonPrefixes "foobar" "foobaz" == Just ("fooba", "r", "z")@
commonPrefixes :: Text -> Text -> Maybe (Text, Text, Text)
commonPrefixes = T.commonPrefixes
{-# INLINE commonPrefixes #-}

-- | @insertWith f key new m@ — if @key@ exists with value @old@, store @f new old@;
-- otherwise insert @new@. Implemented via Map.lookup/insert to avoid GHC's
-- internal Data.Map.Strict.insertWith unfoldings (same safety rationale as
-- local 'alter').
insertWith :: Ord k => (a -> a -> a) -> k -> a -> Map k a -> Map k a
insertWith f k v m = case Map.lookup k m of
  Just old -> let !combined = f v old in Map.insert k combined m
  Nothing  -> Map.insert k v m
{-# INLINE insertWith #-}
