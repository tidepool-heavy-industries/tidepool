{-# LANGUAGE BangPatterns #-}
{-# LANGUAGE MagicHash #-}
{-# LANGUAGE UnboxedTuples #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE RankNTypes #-}
-- | Drop-in replacement for @Data.Text@ as a tidepool HOME module.
--
-- WHY: the @(Char -> Bool)@-taking Data.Text functions, when reached through a
-- home-module binding (a Prelude shadow, a @.tidepool/lib@ verb, or a user
-- helper) with an operator-section predicate, silently corrupt under the JIT —
-- the section's evidence is lost crossing into the EXTERNAL-package interface
-- unfolding (`takewhile-shadow-load-bearing`, `text-vendor-mechanism-proven`).
-- A HOME-module body translates on the same in-session Core path the EPS
-- unpoison fixed for direct use, so it is correct. This module re-exports all
-- of Data.Text and overrides the predicate-class functions with text-2.1.2's
-- bodies compiled here. The @Text@ TYPE and all instances stay EXTERNAL — only
-- the function bodies move home (the bug is in the unfoldings, not the repr).
--
-- Crucially, predicate-applying HELPERS (@span_@, @filter_@,
-- @findAIndexOrEnd@) are vendored LOCALLY too: delegating a section predicate to
-- an external helper is the same red pattern, so every site that applies the
-- user predicate does so in home Core. @all@/@any@/@find@/@findIndex@ are direct
-- iter loops (not stream fusion) for the same reason.
--
-- Bodies vendored verbatim from text-2.1.2 (BSD-3-Clause, (c) 2008-2009 Tom
-- Harper, 2009-2011 Bryan O'Sullivan, 2008-2009 Duncan Coutts) except
-- all/any/find/findIndex, written as direct iter loops (the findAIndexOrEnd
-- shape) to keep the predicate application in home Core.
module Tidepool.Data.Text
  ( module Data.Text
  , Pack(..)
  , takeWhile, takeWhileEnd, dropWhile, dropWhileEnd, dropAround
  , span, break, filter, partition
  , find, findIndex, all, any
  , split, groupBy
  ) where

import Prelude hiding (takeWhile, dropWhile, span, break, filter, all, any, null)
import Data.Text hiding
  ( takeWhile, takeWhileEnd, dropWhile, dropWhileEnd, dropAround
  , span, break, filter, partition
  , find, findIndex, all, any
  , split, groupBy, pack )
import Data.Text (null, empty)
import qualified Data.Text as DT (pack)
import Data.Text.Internal (Text(..), text)
import Data.Text.Unsafe (Iter(..), iter, reverseIter, unsafeTail)
import qualified Data.Text.Array as A
import Data.Text.Internal.Encoding.Utf8 (utf8LengthByLeader, chr2, chr3, chr4)
import Data.Text.Internal.Unsafe.Char (unsafeChr8)
import Control.Monad.ST (ST, runST)

-- | Polymorphic @pack@: identity on 'Text', 'Data.Text.pack' on 'String'.
-- Both the qualified @T.pack@ and the unqualified Prelude @pack@ resolve here,
-- so @T.pack (show x)@ is no longer a trap (@show@ returns 'Text', which @pack@
-- accepts as the identity). Single-method class, no error branches — JIT-safe.
-- (Cost: @T.pack \"literalString\"@ is now ambiguous — write the 'Text' literal
-- directly; @T.pack@ on a 'String' VALUE and on @show@-output both work.)
class Pack a where
  pack :: a -> Text

instance Pack String where
  pack = DT.pack
  {-# INLINE pack #-}

instance Pack Text where
  pack = id
  {-# INLINE pack #-}

-- ---------------------------------------------------------------------------
-- takeWhile / dropWhile family (direct iter loops — text-2.1.2 verbatim)
-- ---------------------------------------------------------------------------

takeWhile :: (Char -> Bool) -> Text -> Text
takeWhile p t@(Text arr off len) = loop 0
  where loop !i | i >= len    = t
                | p c         = loop (i+d)
                | otherwise   = text arr off i
            where Iter c d    = iter t i
{-# INLINE [1] takeWhile #-}

takeWhileEnd :: (Char -> Bool) -> Text -> Text
takeWhileEnd p t@(Text arr off len) = loop (len-1) len
  where loop !i !l | l <= 0    = t
                   | p c       = loop (i+d) (l+d)
                   | otherwise = text arr (off+l) (len-l)
            where Iter c d     = reverseIter t i
{-# INLINE [1] takeWhileEnd #-}

dropWhile :: (Char -> Bool) -> Text -> Text
dropWhile p t@(Text arr off len) = loop 0 0
  where loop !i !l | l >= len  = empty
                   | p c       = loop (i+d) (l+d)
                   | otherwise = Text arr (off+i) (len-l)
            where Iter c d     = iter t i
{-# INLINE [1] dropWhile #-}

dropWhileEnd :: (Char -> Bool) -> Text -> Text
dropWhileEnd p t@(Text arr off len) = loop (len-1) len
  where loop !i !l | l <= 0    = empty
                   | p c       = loop (i+d) (l+d)
                   | otherwise = Text arr off l
            where Iter c d     = reverseIter t i
{-# INLINE [1] dropWhileEnd #-}

dropAround :: (Char -> Bool) -> Text -> Text
dropAround p = dropWhile p . dropWhileEnd p
{-# INLINE [1] dropAround #-}

-- ---------------------------------------------------------------------------
-- span / break / split (predicate via LOCAL span_ — home Core)
-- ---------------------------------------------------------------------------

-- Local copy of Data.Text.Internal.Private.span_ so the predicate is applied
-- in home Core (delegating to the external span_ is the red pattern).
span_ :: (Char -> Bool) -> Text -> (# Text, Text #)
span_ p t@(Text arr off len) = (# hd, tl #)
  where hd = text arr off k
        tl = text arr (off+k) (len-k)
        !k = loop 0
        loop !i | i < len && p c = loop (i+d)
                | otherwise      = i
            where Iter c d       = iter t i
{-# INLINE span_ #-}

span :: (Char -> Bool) -> Text -> (Text, Text)
span p t = case span_ p t of
             (# hd, tl #) -> (hd, tl)
{-# INLINE span #-}

break :: (Char -> Bool) -> Text -> (Text, Text)
break p = span (not . p)
{-# INLINE break #-}

split :: (Char -> Bool) -> Text -> [Text]
split p t
    | null t = [empty]
    | otherwise = loop t
    where loop s | null s'   = [l]
                 | otherwise = l : loop (unsafeTail s')
              where (# l, s' #) = span_ (not . p) s
{-# INLINE split #-}

-- ---------------------------------------------------------------------------
-- filter / partition (predicate via LOCAL filter_ — home Core)
-- ---------------------------------------------------------------------------

filter :: (Char -> Bool) -> Text -> Text
filter p = filter_ text p
{-# INLINE [1] filter #-}

partition :: (Char -> Bool) -> Text -> (Text, Text)
partition p t = (filter p t, filter (not . p) t)
{-# INLINE partition #-}

-- Local copy of Data.Text.Internal.Transformation.filter_ (text-2.1.2 verbatim).
filter_ :: forall a. (A.Array -> Int -> Int -> a) -> (Char -> Bool) -> Text -> a
filter_ mkText p = go
  where
    go (Text src o l) = runST $ do
      let !dstLen = min l 64
      dst <- A.new dstLen
      outer dst dstLen o 0
      where
        outer :: forall s. A.MArray s -> Int -> Int -> Int -> ST s a
        outer !dst !dstLen = inner
          where
            inner !srcOff !dstOff
              | srcOff >= o + l = do
                A.shrinkM dst dstOff
                arr <- A.unsafeFreeze dst
                return $ mkText arr 0 dstOff
              | dstOff + 4 > dstLen = do
                let !dstLen' = dstLen + max 4 (min (l + o - srcOff) dstLen)
                dst' <- A.resizeM dst dstLen'
                outer dst' dstLen' srcOff dstOff
              | otherwise = do
                let m0 = A.unsafeIndex src srcOff
                    m1 = A.unsafeIndex src (srcOff + 1)
                    m2 = A.unsafeIndex src (srcOff + 2)
                    m3 = A.unsafeIndex src (srcOff + 3)
                    !d = utf8LengthByLeader m0
                case d of
                  1 -> do
                    let !c = unsafeChr8 m0
                    if not (p c) then inner (srcOff + 1) dstOff else do
                      A.unsafeWrite dst dstOff m0
                      inner (srcOff + 1) (dstOff + 1)
                  2 -> do
                    let !c = chr2 m0 m1
                    if not (p c) then inner (srcOff + 2) dstOff else do
                      A.unsafeWrite dst dstOff m0
                      A.unsafeWrite dst (dstOff + 1) m1
                      inner (srcOff + 2) (dstOff + 2)
                  3 -> do
                    let !c = chr3 m0 m1 m2
                    if not (p c) then inner (srcOff + 3) dstOff else do
                      A.unsafeWrite dst dstOff m0
                      A.unsafeWrite dst (dstOff + 1) m1
                      A.unsafeWrite dst (dstOff + 2) m2
                      inner (srcOff + 3) (dstOff + 3)
                  _ -> do
                    let !c = chr4 m0 m1 m2 m3
                    if not (p c) then inner (srcOff + 4) dstOff else do
                      A.unsafeWrite dst dstOff m0
                      A.unsafeWrite dst (dstOff + 1) m1
                      A.unsafeWrite dst (dstOff + 2) m2
                      A.unsafeWrite dst (dstOff + 3) m3
                      inner (srcOff + 4) (dstOff + 4)
{-# INLINE filter_ #-}

-- ---------------------------------------------------------------------------
-- all / any / find / findIndex (direct iter loops — predicate in home Core,
-- NOT stream fusion which would delegate the predicate to S.all/S.any/etc.)
-- ---------------------------------------------------------------------------

all :: (Char -> Bool) -> Text -> Bool
all p t@(Text _arr _off len) = go 0
  where go !i | i >= len  = True
              | p c       = go (i+d)
              | otherwise = False
          where Iter c d  = iter t i
{-# INLINE [1] all #-}

any :: (Char -> Bool) -> Text -> Bool
any p t@(Text _arr _off len) = go 0
  where go !i | i >= len  = False
              | p c       = True
              | otherwise = go (i+d)
          where Iter c d  = iter t i
{-# INLINE [1] any #-}

find :: (Char -> Bool) -> Text -> Maybe Char
find p t@(Text _arr _off len) = go 0
  where go !i | i >= len  = Nothing
              | p c       = Just c
              | otherwise = go (i+d)
          where Iter c d  = iter t i
{-# INLINE [1] find #-}

-- Returns the CHAR index (matching Data.Text.findIndex via stream fusion).
findIndex :: (Char -> Bool) -> Text -> Maybe Int
findIndex p t@(Text _arr _off len) = go 0 0
  where go !i !ci | i >= len  = Nothing
                  | p c       = Just ci
                  | otherwise = go (i+d) (ci+1)
              where Iter c d  = iter t i
{-# INLINE [1] findIndex #-}

-- ---------------------------------------------------------------------------
-- groupBy (predicate via LOCAL findAIndexOrEnd — home Core)
-- ---------------------------------------------------------------------------

groupBy :: (Char -> Char -> Bool) -> Text -> [Text]
groupBy p = loop
  where
    loop t@(Text arr off len)
        | null t    = []
        | otherwise = text arr off n : loop (text arr (off+n) (len-n))
        where Iter c d = iter t 0
              n        = d + findAIndexOrEnd (not . p c) (Text arr (off+d) (len-d))

-- Local copy of Data.Text's findAIndexOrEnd (text-2.1.2 verbatim).
findAIndexOrEnd :: (Char -> Bool) -> Text -> Int
findAIndexOrEnd q t@(Text _arr _off len) = go 0
    where go !i | i >= len || q c = i
                | otherwise       = go (i+d)
                where Iter c d    = iter t i
