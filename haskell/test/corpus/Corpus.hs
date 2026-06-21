{-# LANGUAGE GADTs #-}
{-# LANGUAGE ScopedTypeVariables #-}

-- | Real-Core differential corpus. Each top-level binding is a TOTAL program
-- whose -O2 closed Core targets a specific shape that synthetic Core can't reach.
-- Captured via `tidepool-extract-bin --all-closed` (native-bignum binary for the
-- Integer subset) and replayed through `check_jit_vs_eval_captured`.
--
-- Grouped by family. NOINLINE seeds block GHC's constant-folder so the real
-- conversion/parse Core survives to runtime (a folded literal tests nothing).
module Corpus where

import Data.List (foldl', nub, sort)
import Data.Maybe (mapMaybe)
import Data.Ratio ((%))

-- NOINLINE seeds — opaque inputs that defeat constant-folding.
{-# NOINLINE seedI5 #-}
seedI5 :: Integer
seedI5 = 5
{-# NOINLINE seedI1025 #-}
seedI1025 :: Integer
seedI1025 = 1025
{-# NOINLINE seed42 #-}
seed42 :: String
seed42 = "42"
{-# NOINLINE seedPi #-}
seedPi :: String
seedPi = "3.14"
{-# NOINLINE seedList #-}
seedList :: String
seedList = "[1,2,3]"
{-# NOINLINE seedD #-}
seedD :: Double
seedD = 3.75

-- ── Family A: Integer/Rational → Double conversion (#1 constructor-tag class) ──
-- These route through GHC.Float integerToBinaryFloat'/roundingMode#.

convFromInt5 :: Double
convFromInt5 = fromIntegral seedI5

convFromInt1025 :: Double
convFromInt1025 = fromIntegral seedI1025

convFromIntPow40 :: Double
convFromIntPow40 = fromIntegral (2 ^ (40 :: Int) :: Integer)

convFromIntPow80 :: Double
convFromIntPow80 = fromIntegral (2 ^ (80 :: Int) :: Integer)

convFromRational :: Double
convFromRational = fromRational (3 % 8)

convDoubleLitBig :: Double
convDoubleLitBig = 1.0e308

convRealToFrac :: Double
convRealToFrac = realToFrac (fromIntegral seedI1025 :: Double)

-- Double → Integer direction (properFraction-based).
convFloor :: Integer
convFloor = floor seedD

convCeiling :: Integer
convCeiling = ceiling seedD

convRound :: Integer
convRound = round (seedD + 0.25)

convTruncate :: Integer
convTruncate = truncate seedD

convProperFraction :: Integer
convProperFraction = fst (properFraction seedD :: (Integer, Double))

-- ── Family B: field-holds-function / CPS (#2 newtype-coercion class) ──

readInt :: Int
readInt = read seed42

readDouble :: Double
readDouble = read seedPi

readListInt :: [Int]
readListInt = read seedList

newtypeFn :: Int
newtypeFn = case wrap of Endo f -> f 41
  where
    wrap = Endo (\x -> x + 1)

recordClosures :: Int
recordClosures = op1 ops 10 + op2 ops 20
  where
    ops = Ops (+ 1) (* 2)

contMonad :: Int
contMonad = runCont (bindC (retC 5) (\x -> retC (x + 1))) id

-- ── Family C: joinrec (recursive join points) ──

sumRange :: Int
sumRange = sum [1 .. 100]

foldlSum :: Int
foldlSum = foldl' (+) 0 [1 .. 100]

manualLoop :: Int
manualLoop = go 0 1000
  where
    go acc 0 = acc
    go acc n = go (acc + n) (n - 1)

mutualEven :: Bool
mutualEven = isEven (100 :: Int)
  where
    isEven 0 = True
    isEven n = isOdd (n - 1)
    isOdd 0 = False
    isOdd n = isEven (n - 1)

-- ── Family D: GADT / typeclass dispatch / specialization ──

showInt :: String
showInt = show (42 :: Int)

showListInt :: String
showListInt = show [1, 2, 3 :: Int]

customClass :: Int
customClass = size ("hello" :: String) + size [True, False]

gadtEval :: Int
gadtEval = evalE (AddE (IntE 2) (IntE 40))

enumRoundTrip :: Int
enumRoundTrip = fromEnum (toEnum 65 :: Char)

-- ── Family E: Known-Limits checklist + misc constructor shapes ──

takeWhileList :: [Int]
takeWhileList = takeWhile (< 5) [1 .. 10]

nubList :: [Int]
nubList = nub [1, 1, 2, 3, 3, 2]

sortList :: [Int]
sortList = sort [3, 1, 2, 5, 4]

sqrtD :: Double
sqrtD = sqrt 16.0

showDoubleB :: String
showDoubleB = show (3.14 :: Double)

cycleTake :: [Int]
cycleTake = take 3 (cycle [1, 2, 3])

zipWithIdx :: [Int]
zipWithIdx = zipWith (*) [0 ..] [10, 20, 30]

mapMaybeEven :: [Int]
mapMaybeEven = mapMaybe (\x -> if even x then Just (x * x) else Nothing) [1 .. 6]

maybeChain :: Int
maybeChain = maybe 0 (+ 1) (Just 41)

eitherChain :: Int
eitherChain = either negate (+ 1) (Right 41 :: Either Int Int)

-- ── Family F: numeric / Integer arithmetic (non-conversion) ──

intArith :: Int
intArith = (10 * 7 - 3) `div` 2 + 100 `mod` 7

integerShow :: String
integerShow = show (2 ^ (100 :: Int) :: Integer)

integerProduct :: Integer
integerProduct = product [1 .. 20]

gcdLcm :: Int
gcdLcm = gcd 48 36 + lcm 4 6

wordArith :: Int
wordArith = fromIntegral ((maxBound :: Word) `div` 3)

negAbs :: Int
negAbs = abs (negate 17) + signum (-5)

floatArith :: Double
floatArith = (3.5 * 2.0 - 1.0) / 0.5

-- ── Family G: constructor-repr (strict fields, nested ADTs, Ord) ──

strictPair :: Int
strictPair = case SP 3 4 of SP a b -> a * b

treeSum :: Int
treeSum = sumTree (Branch (Leaf 1) (Branch (Leaf 2) (Leaf 3)))

ordCompare :: Bool
ordCompare = compare (3 :: Int) 5 == LT && max 'a' 'z' == 'z'

nestedMaybe :: Int
nestedMaybe = case Just (Just 7) of
  Just (Just x) -> x
  _ -> 0

-- ── Family H: more Known-Limits / text ──

charClass :: Int
charClass = length (filter isDigitC "a1b2c3")
  where
    isDigitC c = c >= '0' && c <= '9'

scanlAccum :: [Int]
scanlAccum = scanl (+) 0 [1, 2, 3, 4]

concatMapList :: [Int]
concatMapList = concatMap (\x -> [x, x * 10]) [1, 2, 3]

unwordsJoin :: String
unwordsJoin = unwords ["hello", "world", "foo"]

-- ── supporting decls (not captured as bindings themselves) ──

data StrictPair = SP !Int !Int

data Tree = Leaf Int | Branch Tree Tree

sumTree :: Tree -> Int
sumTree (Leaf n) = n
sumTree (Branch l r) = sumTree l + sumTree r

newtype Endo = Endo (Int -> Int)

data Ops = Ops {op1 :: Int -> Int, op2 :: Int -> Int}

newtype Cont r a = Cont {runCont :: (a -> r) -> r}

retC :: a -> Cont r a
retC x = Cont (\k -> k x)

bindC :: Cont r a -> (a -> Cont r b) -> Cont r b
bindC (Cont c) f = Cont (\k -> c (\x -> runCont (f x) k))

class Sized a where
  size :: a -> Int

instance Sized [b] where
  size = length

data Expr a where
  IntE :: Int -> Expr Int
  AddE :: Expr Int -> Expr Int -> Expr Int

evalE :: Expr a -> a
evalE (IntE n) = n
evalE (AddE a b) = evalE a + evalE b
