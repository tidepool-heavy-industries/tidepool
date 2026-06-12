{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable #-}
{-# LANGUAGE QuasiQuotes #-}
module Suite where

import Prelude
import qualified Data.Text as T
-- qq-suite: regen now needs `--include lib --target-module-only
-- --output-dir test/suite_cbor` (see CLAUDE.md / plans/qq-spike.md)
import Tidepool.QQ (fmt, j)
-- render lives in lens-free Tidepool.Render (re-exported by Tidepool.Prelude);
-- Suite.hs cannot import Tidepool.Prelude here (it pulls Control.Lens, which
-- the --all-closed extract session cannot see), so import render directly.
import Tidepool.Render (render)
-- Spec'd [fmt|{expr:spec}|] holes expand to calls into Tidepool.QQ.Fmt.Runtime
-- (FSign/FAlign + the fmt* helpers). Like render, these live in a lens-free
-- module so the --all-closed extract session can import them directly.
import Tidepool.QQ.Fmt.Runtime
-- Value module directly, NOT the Tidepool.Aeson facade: the facade
-- re-exports Tidepool.Aeson.Lens -> Control.Lens, which the extract GHC
-- session cannot see (lens is not a boot package), so the facade kills
-- --all-closed regen at compile time.
import Tidepool.Aeson.Value (Value (..))
type Text = T.Text

-- ============================================================
-- Int literals (5)
-- ============================================================

lit_42 :: Int
lit_42 = 42

lit_zero :: Int
lit_zero = 0

lit_neg7 :: Int
lit_neg7 = -7

lit_large :: Int
lit_large = 1000000

lit_neg_large :: Int
lit_neg_large = -999999

-- ============================================================
-- Other literals (5)
-- ============================================================

lit_char_a :: Char
lit_char_a = 'a'

lit_char_z :: Char
lit_char_z = 'z'

lit_char_newline :: Char
lit_char_newline = '\n'

lit_double_pi :: Double
lit_double_pi = 3.14159

lit_double_neg :: Double
lit_double_neg = -2.5

-- ============================================================
-- Arithmetic (12)
-- ============================================================

add_simple :: Int
add_simple = 1 + 2

sub_simple :: Int
sub_simple = 10 - 3

mul_simple :: Int
mul_simple = 6 * 7

nested_arith :: Int
nested_arith = (3 + 4) * (5 - 2)

arith_precedence :: Int
arith_precedence = 2 + 3 * 4

arith_left_assoc :: Int
arith_left_assoc = 10 - 3 - 2

arith_neg_result :: Int
arith_neg_result = 3 - 10

arith_mul_zero :: Int
arith_mul_zero = 42 * 0

arith_mul_one :: Int
arith_mul_one = 42 * 1

arith_double_add :: Double
arith_double_add = 1.5 + 2.5

arith_double_mul :: Double
arith_double_mul = 3.0 * 2.0

arith_double_sub :: Double
arith_double_sub = 10.0 - 3.5

-- ============================================================
-- Comparisons (8)
-- ============================================================

cmp_eq_true :: Bool
cmp_eq_true = (5 :: Int) == 5

cmp_eq_false :: Bool
cmp_eq_false = (5 :: Int) == 6

cmp_ne_true :: Bool
cmp_ne_true = (5 :: Int) /= 6

cmp_lt_true :: Bool
cmp_lt_true = (3 :: Int) < 5

cmp_lt_false :: Bool
cmp_lt_false = (5 :: Int) < 3

cmp_gt_true :: Bool
cmp_gt_true = (5 :: Int) > 3

cmp_le_eq :: Bool
cmp_le_eq = (5 :: Int) <= 5

cmp_ge_eq :: Bool
cmp_ge_eq = (5 :: Int) >= 5

-- ============================================================
-- Let bindings (8)
-- ============================================================

let_simple :: Int
let_simple = let x = 10 in x

let_two :: Int
let_two = let x = 10; y = 20 in x + y

let_nested :: Int
let_nested = let x = let y = 5 in y + 1 in x * 2

let_shadow :: Int
let_shadow = let x = 10 in let x = 20 in x

let_unused :: Int
let_unused = let x = 99 in let _y = 100 in x

let_chain :: Int
let_chain = let a = 1; b = a + 1; c = b + 1 in c

let_complex :: Int
let_complex = let x = 3 * 4 in let y = x + 1 in y * 2

let_body_only :: Int
let_body_only = let _unused = 999999 in 42

-- ============================================================
-- LetRec (8)
-- ============================================================

letrec_fact5 :: Int
letrec_fact5 =
  let go :: Int -> Int -> Int
      go n acc = if n <= 0 then acc else go (n - 1) (n * acc)
  in go 5 1

letrec_fib10 :: Int
letrec_fib10 =
  let fib :: Int -> Int
      fib n = if n <= 1 then n else fib (n - 1) + fib (n - 2)
  in fib 10

letrec_countdown :: Int
letrec_countdown =
  let go :: Int -> Int
      go n = if n <= 0 then 0 else go (n - 1)
  in go 10

letrec_sum_to :: Int
letrec_sum_to =
  let go :: Int -> Int -> Int
      go n acc = if n <= 0 then acc else go (n - 1) (acc + n)
  in go 10 0

letrec_pow :: Int
letrec_pow =
  let pow' :: Int -> Int -> Int
      pow' b e = if e <= 0 then 1 else b * pow' b (e - 1)
  in pow' 2 10

letrec_gcd :: Int
letrec_gcd =
  let gcd' :: Int -> Int -> Int
      gcd' a b = if b == 0 then a
                 else if a >= b then gcd' (a - b) b
                 else gcd' b a
  in gcd' 48 18

letrec_even_odd :: Bool
letrec_even_odd =
  let isEven :: Int -> Bool
      isEven n = if n == 0 then True else isOdd (n - 1)
      isOdd :: Int -> Bool
      isOdd n  = if n == 0 then False else isEven (n - 1)
  in isEven 10

letrec_ackermann :: Int
letrec_ackermann =
  let ack :: Int -> Int -> Int
      ack m n = if m == 0 then n + 1
                else if n == 0 then ack (m - 1) 1
                else ack (m - 1) (ack m (n - 1))
  in ack 2 3

-- ============================================================
-- Case / pattern match (15)
-- ============================================================

case_just :: Int
case_just = case Just (42 :: Int) of { Nothing -> 0; Just x -> x }

case_nothing :: Int
case_nothing = case (Nothing :: Maybe Int) of { Nothing -> 0; Just x -> x }

case_true :: Int
case_true = case True of { True -> 1; False -> 0 }

case_false :: Int
case_false = case False of { True -> 1; False -> 0 }

case_left :: Int
case_left = case (Left 10 :: Either Int Int) of { Left x -> x; Right y -> y }

case_right :: Int
case_right = case (Right 20 :: Either Int Int) of { Left x -> x; Right y -> y }

case_nested_just :: Int
case_nested_just = case Just (Just (99 :: Int)) of
  Nothing -> 0
  Just inner -> case inner of
    Nothing -> 0
    Just x -> x

case_pair :: Int
case_pair = case (10 :: Int, 20 :: Int) of { (a, b) -> a + b }

case_triple :: Int
case_triple = case (1 :: Int, 2 :: Int, 3 :: Int) of { (a, b, c) -> a + b + c }

case_default :: Int
case_default = case (42 :: Int) of { _ -> 99 }

case_bool_and :: Bool
case_bool_and = case (True, True) of { (True, True) -> True; _ -> False }

case_bool_or :: Bool
case_bool_or = case (False, True) of { (False, False) -> False; _ -> True }

case_nested_case :: Int
case_nested_case = case Just True of
  Nothing -> 0
  Just b -> case b of
    True -> 1
    False -> 2

case_either_nested :: Int
case_either_nested = case (Right (Just (7 :: Int)) :: Either Int (Maybe Int)) of
  Left x -> x
  Right mb -> case mb of
    Nothing -> 0
    Just x -> x

case_wildcard_pair :: Int
case_wildcard_pair = case (10 :: Int, 20 :: Int) of { (_, b) -> b }

-- ============================================================
-- Data constructors (10)
-- ============================================================

con_just :: Maybe Int
con_just = Just 42

con_nothing :: Maybe Int
con_nothing = Nothing

con_pair :: (Int, Int)
con_pair = (10, 20)

con_triple :: (Int, Int, Int)
con_triple = (1, 2, 3)

con_left :: Either Int Int
con_left = Left 10

con_right :: Either Int Int
con_right = Right 20

con_nested_just :: Maybe (Maybe Int)
con_nested_just = Just (Just 99)

con_nested_nothing :: Maybe (Maybe Int)
con_nested_nothing = Just Nothing

con_true :: Bool
con_true = True

con_false :: Bool
con_false = False

-- ============================================================
-- Lambda / application (8)
-- ============================================================

app_identity :: Int
app_identity = (\x -> x) 42

app_const :: Int
app_const = (\x _ -> x) (10 :: Int) (20 :: Int)

app_compose :: Int
app_compose = (\f g x -> f (g x)) (+ (1 :: Int)) (* (2 :: Int)) 5

app_nested_lam :: Int
app_nested_lam = (\x -> \y -> x + y) (3 :: Int) (4 :: Int)

app_thrice :: Int
app_thrice = (\f x -> f (f (f x))) (+ (1 :: Int)) 0

app_twice :: Int
app_twice = let twice f x = f (f x) in twice (+ (3 :: Int)) 10

app_church_zero :: Int
app_church_zero = let zero _f x = x in zero (+ (1 :: Int)) (0 :: Int)

app_church_two :: Int
app_church_two = let two f x = f (f x) in two (+ (1 :: Int)) (0 :: Int)

-- ============================================================
-- Higher-order (hand-written, no Prelude) (8)
-- ============================================================

ho_mymap_len :: Int
ho_mymap_len =
  let mylen acc [] = acc
      mylen acc (_:xs) = mylen (acc + 1) xs
      mymap _ [] = []
      mymap f (x:xs) = f x : mymap f xs
  in mylen 0 (mymap (+ (1 :: Int)) [1 :: Int, 2, 3])

ho_myfoldr :: Int
ho_myfoldr =
  let myfoldr _ z [] = z
      myfoldr f z (x:xs) = f x (myfoldr f z xs)
  in myfoldr (+) (0 :: Int) [1 :: Int, 2, 3, 4, 5]

ho_myfoldl :: Int
ho_myfoldl =
  let myfoldl _ z [] = z
      myfoldl f z (x:xs) = myfoldl f (f z x) xs
  in myfoldl (+) (0 :: Int) [1 :: Int, 2, 3, 4, 5]

ho_myfilter_len :: Int
ho_myfilter_len =
  let mylen acc [] = acc
      mylen acc (_:xs) = mylen (acc + 1) xs
      myfilter _ [] = []
      myfilter p (x:xs) = if p x then x : myfilter p xs else myfilter p xs
      myeven n = let go k = if k >= n then k == n else go (k + 2) in go (0 :: Int)
  in mylen 0 (myfilter myeven [1 :: Int, 2, 3, 4, 5, 6])

ho_myany :: Bool
ho_myany =
  let myany _ [] = False
      myany p (x:xs) = if p x then True else myany p xs
      myeven n = let go k = if k >= n then k == n else go (k + 2) in go (0 :: Int)
  in myany myeven [1 :: Int, 2, 3]

ho_myall :: Bool
ho_myall =
  let myall _ [] = True
      myall p (x:xs) = if p x then myall p xs else False
      myeven n = let go k = if k >= n then k == n else go (k + 2) in go (0 :: Int)
  in myall myeven [1 :: Int, 2, 3]

ho_myzipwith :: Int
ho_myzipwith =
  let myzipwith _ [] _ = []
      myzipwith _ _ [] = []
      myzipwith f (a:as') (b:bs) = f a b : myzipwith f as' bs
      myhead (x:_) = x
      myhead [] = 0
  in myhead (myzipwith (+) [1 :: Int, 2, 3] [10, 20, 30])

ho_myconcatmap :: Int
ho_myconcatmap =
  let myconcat [] = []
      myconcat (xs:xss) = myappend xs (myconcat xss)
      myappend [] ys = ys
      myappend (x:xs) ys = x : myappend xs ys
      mymap _ [] = []
      mymap f (x:xs) = f x : mymap f xs
      mylen acc [] = acc
      mylen acc (_:xs) = mylen (acc + 1) xs
  in mylen 0 (myconcat (mymap (\x -> [x, x * 2]) [1 :: Int, 2, 3]))

-- ============================================================
-- If-then-else / guards (5)
-- ============================================================

ite_simple :: Int
ite_simple = if True then 1 else 0

ite_false :: Int
ite_false = if False then 1 else 0

ite_nested :: Int
ite_nested = if True then (if False then 1 else 2) else 3

ite_abs :: Int
ite_abs = let myabs x = if x < 0 then negate x else x in myabs (-5 :: Int)

ite_signum :: Int
ite_signum =
  let mysignum x = if x > 0 then 1 else if x < 0 then -1 else 0
  in mysignum (-42 :: Int)

-- ============================================================
-- Edge cases (8)
-- ============================================================

edge_deep_let :: Int
edge_deep_let =
  let a = 1 :: Int
      b = a + 1
      c = b + 1
      d = c + 1
      e = d + 1
      f = e + 1
      g = f + 1
      h = g + 1
      i = h + 1
      j = i + 1
  in j

edge_large_tuple :: Int
edge_large_tuple = case (1 :: Int, 2 :: Int, 3 :: Int, 4 :: Int, 5 :: Int) of
  (a, b, c, d, e) -> a + b + c + d + e

edge_nullary_con :: Bool
edge_nullary_con = case False of { False -> True; True -> False }

edge_id_chain :: Int
edge_id_chain = (\x -> x) ((\x -> x) ((\x -> x) ((\x -> x) (42 :: Int))))

edge_const_chain :: Int
edge_const_chain = (\x _ -> x) ((\x _ -> x) ((\x _ -> x) (42 :: Int) 'a') "hello") True

edge_case_of_case :: Int
edge_case_of_case =
  case (case True of { True -> Just (1 :: Int); False -> Nothing }) of
    Nothing -> 0
    Just x -> x

edge_deep_nesting :: Int
edge_deep_nesting =
  let f x = let g y = let h z = x + y + z in h 3 in g 2
  in f (1 :: Int)

edge_mutual_data :: Int
edge_mutual_data =
  let wrap x = Just x
      unwrap mb = case mb of { Nothing -> 0; Just x -> x }
  in unwrap (wrap (42 :: Int))

-- ============================================================
-- Prelude functions (closure resolution) (10)
-- ============================================================

prelude_null_empty :: Bool
prelude_null_empty = null ([] :: [Int])

prelude_null_nonempty :: Bool
prelude_null_nonempty = null [1 :: Int, 2, 3]

prelude_length :: Int
prelude_length = length [1 :: Int, 2, 3, 4, 5]

prelude_take :: Int
prelude_take =
  let mylen acc [] = acc
      mylen acc (_:xs) = mylen (acc + 1) xs
  in mylen 0 (take 3 [1 :: Int, 2, 3, 4, 5])

prelude_map :: Int
prelude_map =
  let mylen acc [] = acc
      mylen acc (_:xs) = mylen (acc + 1) xs
  in mylen 0 (map (+ (1 :: Int)) [1 :: Int, 2, 3])

prelude_filter :: Int
prelude_filter =
  let mylen acc [] = acc
      mylen acc (_:xs) = mylen (acc + 1) xs
      isEven n = let go k = if k >= n then k == n else go (k + 2) in go (0 :: Int)
  in mylen 0 (filter isEven [1 :: Int, 2, 3, 4, 5, 6])

prelude_or :: Bool
prelude_or = False || True

prelude_and :: Bool
prelude_and = True && False

prelude_eq_int :: Bool
prelude_eq_int = (42 :: Int) == 42

prelude_string_append :: Int
prelude_string_append =
  let mylen acc [] = acc
      mylen acc (_:xs) = mylen (acc + 1) xs
  in mylen 0 ("hello" ++ " world")

-- Test take on cons-chain (not string literal to avoid indexCharOffAddr# fusion)
prelude_take_cons :: Int
prelude_take_cons =
  let mylen acc [] = acc
      mylen acc (_:xs) = mylen (acc + 1) xs
      s = 'h' : 'e' : 'l' : 'l' : 'o' : []
  in mylen 0 (take 3 s)

-- Test == on cons-chain strings (not literals to avoid indexCharOffAddr# fusion)
prelude_eq_string_true :: Bool
prelude_eq_string_true =
  let s1 = 'h' : 'e' : 'l' : 'l' : 'o' : []
      s2 = 'h' : 'e' : 'l' : 'l' : 'o' : []
  in s1 == s2

prelude_eq_string_false :: Bool
prelude_eq_string_false =
  let s1 = 'h' : 'e' : 'l' : 'l' : 'o' : []
      s2 = 'w' : 'o' : 'r' : 'l' : 'd' : []
  in s1 == s2

-- ============================================================
-- Multi-return primops (2)
-- ============================================================

prim_quot_rem_int :: Int
prim_quot_rem_int =
  let (q, r) = quotRem (10 :: Int) (3 :: Int)
  in q * 10 + r -- should be 3 * 10 + 1 = 31

prim_quot_rem_word :: Int
prim_quot_rem_word =
  let (q, r) = quotRem (10 :: Word) (3 :: Word)
  in fromIntegral (q * 10 + r) -- should be 3 * 10 + 1 = 31

-- ============================================================
-- Show (7)
-- ============================================================

showInt :: String
showInt = show (42 :: Int)

showIntNeg :: String
showIntNeg = show (-7 :: Int)

showCharA :: String
showCharA = show ('a' :: Char)

showHello :: String
showHello = show ("hello" :: String)

showMaybeInt :: String
showMaybeInt = show (Just 42 :: Maybe Int)

showMaybeNothing :: String
showMaybeNothing = show (Nothing :: Maybe Int)

showBool :: String
showBool = show True

showDouble :: String
showDouble = showDouble' (3.14 :: Double)

showDoubleInt :: String
showDoubleInt = showDouble' (42.0 :: Double)

-- Fallback body uses `d` so GHC preserves the argument in Core.
-- GHC eta-reduces this to $fShowDouble_$cshow, which resolveExternals
-- skips (isMagicUnpackVar) and Translate.hs intercepts (isShowDoubleVar).
{-# NOINLINE showDouble' #-}
showDouble' :: Double -> String
showDouble' d = show d

showDoubleText :: T.Text
showDoubleText = T.pack (showDouble' (3.14 :: Double))

-- Use Prelude's show directly (not showDouble') to test the GHC compilation path
showDoublePrelude :: String
showDoublePrelude = show (3.14 :: Double)

showDoublePreludeText :: T.Text
showDoublePreludeText = T.pack (show (3.14 :: Double))

-- ============================================================
-- Lazy thunk tests (8)
-- ============================================================

-- Infinite list producers
thunk_repeat :: [Int]
thunk_repeat = take 5 (repeat 1)

thunk_iterate :: [Int]
thunk_iterate = take 5 (iterate (+1) 0)

thunk_cycle :: [Int]
thunk_cycle = take 7 (cycle [1, 2, 3])

-- Multi-input consumer (motivating case)
thunk_zipwith :: [Int]
thunk_zipwith = zipWith (+) [10, 20, 30] [0..]

thunk_zipwith_inf :: [Int]
thunk_zipwith_inf = take 4 (zipWith (+) [0..] [100..])

thunk_map_inf :: [Int]
thunk_map_inf = take 5 (map (*2) [0..])

-- BlackHole detection
thunk_blackhole :: Int
thunk_blackhole = let x = x in x

-- LetRec knot-tying still works
thunk_letrec_knot :: [Int]
thunk_letrec_knot = let xs = 1 : xs in take 5 xs

-- ============================================================
-- Text takeWhile/dropWhile PAP regression tests
-- ============================================================

-- Pure reimplementations (same as Tidepool.Prelude.takeWhileT/dropWhileT)
myTakeWhileT :: (Char -> Bool) -> T.Text -> T.Text
myTakeWhileT p t = T.pack (go (T.unpack t))
  where
    go [] = []
    go (c:cs)
      | p c       = c : go cs
      | otherwise = []

myDropWhileT :: (Char -> Bool) -> T.Text -> T.Text
myDropWhileT p t = T.pack (go (T.unpack t))
  where
    go [] = []
    go s@(c:cs)
      | p c       = go cs
      | otherwise = s

-- Direct application (should work regardless)
text_takeWhileT_direct :: T.Text
text_takeWhileT_direct = myTakeWhileT (/= '/') (T.pack "hello/world")

-- Point-free via map (this is the PAP bug trigger pattern)
text_takeWhileT_map :: [T.Text]
text_takeWhileT_map = map (myTakeWhileT (/= '/')) [T.pack "hello/world", T.pack "foo/bar", T.pack "noSlash"]

-- Eta-expanded via map (reference — should produce same result as above)
text_takeWhileT_eta :: [T.Text]
text_takeWhileT_eta = map (\p -> myTakeWhileT (/= '/') p) [T.pack "hello/world", T.pack "foo/bar", T.pack "noSlash"]

-- Direct application
text_dropWhileT_direct :: T.Text
text_dropWhileT_direct = myDropWhileT (/= '/') (T.pack "hello/world")

-- Point-free via map
text_dropWhileT_map :: [T.Text]
text_dropWhileT_map = map (myDropWhileT (/= '/')) [T.pack "hello/world", T.pack "foo/bar", T.pack "noSlash"]

-- Eta-expanded via map (reference)
text_dropWhileT_eta :: [T.Text]
text_dropWhileT_eta = map (\p -> myDropWhileT (/= '/') p) [T.pack "hello/world", T.pack "foo/bar", T.pack "noSlash"]

-- ============================================================
-- Lazy filter and nubBy regression tests
-- ============================================================

lazy_filter_infinite :: Int
lazy_filter_infinite =
  let myfilter _ [] = []
      myfilter p (x:xs)
        | p x       = x : myfilter p xs
        | otherwise = myfilter p xs
      naturals = go (0 :: Int) where go n = n : go (n + (1 :: Int))
      isEven (n :: Int) = (n `rem` (2 :: Int)) == (0 :: Int)
      mySum [] = 0 :: Int
      mySum (x:xs) = x + mySum xs
  in mySum (take (5 :: Int) (myfilter isEven naturals))

lazy_nubby_infinite :: Int
lazy_nubby_infinite =
  let mynubBy eq = go []
        where
          go _ [] = []
          go seen (x:rest)
            | elemBy x seen = go seen rest
            | otherwise     = x : go (x : seen) rest
          elemBy _ []     = False
          elemBy x (y:ys)
            | eq x y    = True
            | otherwise = elemBy x ys
      naturals = go (0 :: Int) where go n = n : go (n + (1 :: Int))
      myEq (a :: Int) (b :: Int) = a == b
      myLen [] = 0 :: Int
      myLen (_:xs) = (1 :: Int) + myLen xs
  in myLen (take (5 :: Int) (mynubBy myEq naturals))

nubby_dedup_finite :: Int
nubby_dedup_finite =
  let mynubBy eq = go []
        where
          go _ [] = []
          go seen (x:rest)
            | elemBy x seen = go seen rest
            | otherwise     = x : go (x : seen) rest
          elemBy _ []     = False
          elemBy x (y:ys)
            | eq x y    = True
            | otherwise = elemBy x ys
      myEq (a :: Int) (b :: Int) = a == b
      myLen [] = 0 :: Int
      myLen (_:xs) = (1 :: Int) + myLen xs
  in myLen (mynubBy myEq [1 :: Int, 2, 1, 3, 2, 4])

filter_order_preserved :: Int
filter_order_preserved =
  let myfilter _ [] = []
      myfilter p (x:xs)
        | p x       = x : myfilter p xs
        | otherwise = myfilter p xs
      naturals = go (0 :: Int) where go n = n : go (n + (1 :: Int))
      isOdd (n :: Int) = (n `rem` (2 :: Int)) /= (0 :: Int)
      toCode (f0:f1:f2:_) = f0 * (100 :: Int) + f1 * (10 :: Int) + f2
      toCode _ = 0 :: Int
  in toCode (myfilter isOdd naturals)

lazy_concatmap_infinite :: Int
lazy_concatmap_infinite =
  let myconcatMap _ [] = []
      myconcatMap f (x:xs) = go (f x)
        where
          go []     = myconcatMap f xs
          go (y:ys) = y : go ys
      naturals = go (0 :: Int) where go n = n : go (n + (1 :: Int))
      dup (x :: Int) = [x, x]
      mySum [] = 0 :: Int
      mySum (x:xs) = x + mySum xs
  in mySum (take (6 :: Int) (myconcatMap dup naturals))

concatmap_finite :: Int
concatmap_finite =
  let myconcatMap _ [] = []
      myconcatMap f (x:xs) = go (f x)
        where
          go []     = myconcatMap f xs
          go (y:ys) = y : go ys
      triple (x :: Int) = [x, x, x]
      myLen [] = 0 :: Int
      myLen (_:xs) = (1 :: Int) + myLen xs
  in myLen (myconcatMap triple [1 :: Int, 2, 3, 4])

concatmap_empty_segments :: Int
concatmap_empty_segments =
  let myconcatMap _ [] = []
      myconcatMap f (x:xs) = go (f x)
        where
          go []     = myconcatMap f xs
          go (y:ys) = y : go ys
      naturals = go (0 :: Int) where go n = n : go (n + (1 :: Int))
      evenOnly (n :: Int) = if (n `rem` (2 :: Int)) == (0 :: Int) then [n] else []
      myLen [] = 0 :: Int
      myLen (_:xs) = (1 :: Int) + myLen xs
  in myLen (myconcatMap evenOnly (take (10 :: Int) naturals))

-- FfiRintDouble (rintDouble FFI -> Cranelift nearest / round_ties_even):
-- base's specialized round @Double @Int now works; Prelude round delegates.
round_banker_half :: Int
round_banker_half = round (2.5 :: Double)

round_banker_threehalf :: Int
round_banker_threehalf = round (3.5 :: Double)

round_simple_up :: Int
round_simple_up = round (3.7 :: Double)

round_negative_half :: Int
round_negative_half = round (-2.5 :: Double)

-- ============================================================
-- prelude-workhorses: tests
-- ============================================================

-- Simple local implementations to avoid adding imports to the top of Suite.hs

sortOn :: Ord b => (a -> b) -> [a] -> [a]
sortOn f = map snd . sortBy (comparing fst) . map (\x -> (f x, x))
  where
    comparing g x y = compare (g x) (g y)
    sortBy _ [] = []
    sortBy cmp (x:xs) =
      let (lesser, greater) = partition (\y -> cmp y x == LT) xs
      in sortBy cmp lesser ++ [x] ++ sortBy cmp greater
    partition _ [] = ([], [])
    partition p (x:xs) =
      let (ys, zs) = partition p xs
      in if p x then (x:ys, zs) else (ys, x:zs)

data Down a = Down a
  deriving (Eq, Show)

instance Ord a => Ord (Down a) where
  compare (Down a) (Down b) = compare b a

down :: a -> Down a
down = Down

swap :: (a, b) -> (b, a)
swap (a, b) = (b, a)

partitionEithers :: [Either a b] -> ([a], [b])
partitionEithers = foldr (either (\a (as, bs) -> (a:as, bs)) (\b (as, bs) -> (as, b:bs))) ([], [])

rights :: [Either a b] -> [b]
rights = foldr (either (const id) (:)) []

lefts :: [Either a b] -> [a]
lefts = foldr (either (:) (const id)) []

fromLeft :: a -> Either a b -> a
fromLeft _ (Left a) = a
fromLeft a (Right _) = a

fromRight :: b -> Either a b -> b
fromRight _ (Right b) = b
fromRight b (Left _) = b

first :: (a -> c) -> (a, b) -> (c, b)
first f (a, b) = (f a, b)

second :: (b -> c) -> (a, b) -> (a, c)
second f (a, b) = (a, f b)

bimap :: (a -> c) -> (b -> d) -> (a, b) -> (c, d)
bimap f g (a, b) = (f a, g b)

t_sortOn :: [(T.Text, Int)]
t_sortOn = sortOn snd [(T.pack "x",3),(T.pack "y",1),(T.pack "z",2)]

t_sortOnDown :: [(T.Text, Int)]
t_sortOnDown = sortOn (down . snd) [(T.pack "x",3),(T.pack "y",1),(T.pack "z",2)]

t_swap :: (T.Text, Int)
t_swap = swap (1::Int, T.pack "s")

t_first :: (Int, T.Text)
t_first = first (+1) (1::Int, T.pack "k")

t_second :: (T.Text, Int)
t_second = second T.length (T.pack "k", T.pack "abc")

t_bimap :: (Int, Int)
t_bimap = bimap (+1) T.length ((1::Int), T.pack "abc")

t_partitionEithers :: ([T.Text], [Int])
t_partitionEithers = partitionEithers [Left (T.pack "a"), Right (1::Int), Left (T.pack "b"), Right 2]

t_rightsLefts :: ([Int], [T.Text])
t_rightsLefts = let xs = [Left (T.pack "a"), Right (1::Int), Left (T.pack "b"), Right 2] in (rights xs, lefts xs)

t_fromEither :: (T.Text, Int)
t_fromEither = (fromLeft (T.pack "d") (Right (1::Int)), fromRight 0 (Right 2::Either T.Text Int))

-- pragma-uplift: extension smoke tests

t_lambdaCase :: [Text]
t_lambdaCase = map (\case { 0 -> "z"; n | n < 0 -> "n"; _ -> "p" }) [-1,0,2]

t_tupleSections :: [(Int, Bool)]
t_tupleSections = map (,True) [1,2,3]

t_multiWayIf :: Text
t_multiWayIf = let x = 5 :: Int in if | x < 0 -> "neg" | x > 0 -> "pos" | otherwise -> "zero"

data RWRecord = RWRecord { rwField1 :: Int, rwField2 :: Text }
t_recordWildCards :: (Int, Text)
t_recordWildCards = let r = RWRecord { rwField1 = 42, rwField2 = "hello" } in let RWRecord{..} = r in (rwField1, rwField2)

data PunRecord = PunRecord { punField :: Int }
t_namedFieldPuns :: Int
t_namedFieldPuns = let punField = 10 in let r = PunRecord { punField } in let PunRecord{punField} = r in punField

t_viewPatterns :: Text
t_viewPatterns = let f (T.toUpper -> u) = u in f "hello"

t_bangPatterns :: Int
t_bangPatterns = let go !acc [] = acc; go !acc (x:xs) = go (acc + x) xs in go 0 [1..10]

t_typeApplications :: Int
t_typeApplications = id @Int 5

t_blockArguments :: Int
t_blockArguments = length do [1,2,3]

t_numericUnderscores :: Int
t_numericUnderscores = 1_000_000

t_multilineStrings :: Int
t_multilineStrings = T.length """
  multi
  line
  """

data Box a = Box a deriving (Functor, Foldable, Traversable)
t_deriveFunctor :: Int
t_deriveFunctor = let Box x = fmap (+1) (Box 41) in x

t_deriveFoldable :: Int
t_deriveFoldable = sum (Box 42)

t_deriveTraversable :: Maybe Int
t_deriveTraversable = fmap (\(Box x) -> x) (traverse (\x -> Just (x + 1)) (Box 41))

-- ============================================================
-- qq-suite: tests
-- ============================================================
-- Two sections below are owned by separate leaves; each leaf adds
-- bindings ONLY inside its own section.

-- ---- qq-suite: fmt section (owner: leaf qq-fmt) ----

qq_fmt_empty :: T.Text
qq_fmt_empty = [fmt||]

qq_fmt_plain :: T.Text
qq_fmt_plain = [fmt|hello, tidepool world|]

qq_fmt_basic :: T.Text
qq_fmt_basic = [fmt|user: {name}|]
  where name = T.pack "alice" :: T.Text

qq_fmt_multi :: T.Text
qq_fmt_multi = [fmt|hello {T.toUpper greeting}, score {score}|]
  where
    greeting = T.pack "world" :: T.Text
    score    = T.pack "42"   :: T.Text

qq_fmt_escape :: T.Text
qq_fmt_escape = [fmt|use \{braces} for holes and } is literal|]

qq_fmt_multiline :: T.Text
qq_fmt_multiline = [fmt|line one
line two|]

-- Render-coerced holes: arbitrary expressions parsed by the vendored GHC
-- parser, each wrapped in `render` (Tidepool.Render) so the hole need not be
-- Text already.

-- Int hole (render @Int): "count: 3"
qq_fmt_int :: T.Text
qq_fmt_int = [fmt|count: {count}|]
  where count = 3 :: Int

-- Operator hole (render @Int over an arithmetic expression): "next: 42"
qq_fmt_op :: T.Text
qq_fmt_op = [fmt|next: {n + 1}|]
  where n = 41 :: Int

-- Applied + operator hole (Text result, render @Text = id): "shout: HI!"
qq_fmt_applied :: T.Text
qq_fmt_applied = [fmt|shout: {T.toUpper s <> T.pack "!"}|]
  where s = T.pack "hi" :: T.Text

-- Double hole (render @Double → ShowDoubleAddr): "val: 3.5"
qq_fmt_double :: T.Text
qq_fmt_double = [fmt|val: {d}|]
  where d = 3.5 :: Double

-- Bool hole (render @Bool): "flag: True"
qq_fmt_bool :: T.Text
qq_fmt_bool = [fmt|flag: {b}|]
  where b = True :: Bool

-- Char hole (render @Char, via show → quoted): "ch: 'x'"
qq_fmt_char :: T.Text
qq_fmt_char = [fmt|ch: {c}|]
  where c = 'x' :: Char

-- ---- qq-suite: fmt format-spec section (Phase 2 — PyF {expr:spec}) ----
--
-- Spec'd holes route through the compile-time interpreter (Tidepool.QQ.Fmt) to
-- monomorphic JIT-safe helpers (Tidepool.QQ.Fmt.Runtime).  Floats use the
-- round primop (no floatToDigits); the differential harness re-runs every
-- fixture on the JIT and checks interpreter ≡ JIT.

-- Fixed-point: the research's proven cases.
qq_fmt_spec_fixed2 :: T.Text          -- {d:.2f} on 3.14159 -> "3.14"
qq_fmt_spec_fixed2 = [fmt|{d:.2f}|] where d = 3.14159 :: Double

qq_fmt_spec_fixed_half :: T.Text      -- 2.5 -> "2.50"
qq_fmt_spec_fixed_half = [fmt|{d:.2f}|] where d = 2.5 :: Double

qq_fmt_spec_fixed_neg :: T.Text       -- -1.2 -> "-1.20"
qq_fmt_spec_fixed_neg = [fmt|{d:.2f}|] where d = -1.2 :: Double

qq_fmt_spec_fixed_small :: T.Text     -- 0.07 -> "0.07"
qq_fmt_spec_fixed_small = [fmt|{d:.2f}|] where d = 0.07 :: Double

-- Integer width / bases.
qq_fmt_spec_zero_pad :: T.Text        -- {n:04d} on 42 -> "0042"
qq_fmt_spec_zero_pad = [fmt|{n:04d}|] where n = 42 :: Int

qq_fmt_spec_hex :: T.Text             -- {n:x} on 255 -> "ff"
qq_fmt_spec_hex = [fmt|{n:x}|] where n = 255 :: Int

qq_fmt_spec_oct :: T.Text             -- {n:o} on 64 -> "100"
qq_fmt_spec_oct = [fmt|{n:o}|] where n = 64 :: Int

qq_fmt_spec_bin :: T.Text             -- {n:b} on 42 -> "101010"
qq_fmt_spec_bin = [fmt|{n:b}|] where n = 42 :: Int

-- Alignment over a Text hole.
qq_fmt_spec_align_right :: T.Text     -- {t:>10} -> "        hi"
qq_fmt_spec_align_right = [fmt|{t:>10}|] where t = T.pack "hi"

qq_fmt_spec_align_left :: T.Text      -- {t:<10} -> "hi        "
qq_fmt_spec_align_left = [fmt|{t:<10}|] where t = T.pack "hi"

qq_fmt_spec_align_center :: T.Text    -- {t:^10} -> "    hi    "
qq_fmt_spec_align_center = [fmt|{t:^10}|] where t = T.pack "hi"

-- Sign and percent.
qq_fmt_spec_sign :: T.Text            -- {n:+} on 42 -> "+42"
qq_fmt_spec_sign = [fmt|{n:+}|] where n = 42 :: Int

qq_fmt_spec_percent :: T.Text         -- {d:%} on 0.5 -> "50.000000%"
qq_fmt_spec_percent = [fmt|{d:%}|] where d = 0.5 :: Double

qq_fmt_spec_percent1 :: T.Text        -- {d:.1%} on 0.5 -> "50.0%"
qq_fmt_spec_percent1 = [fmt|{d:.1%}|] where d = 0.5 :: Double

-- Brace-nesting: a string literal containing '}' inside the hole (the old
-- `break (== '}')` lexer stopped at the first '}'; the new lexer skips the
-- string literal and finds the real closing brace).
qq_fmt_spec_brace :: T.Text           -- -> "a}b"
qq_fmt_spec_brace = [fmt|{T.pack "a}b"}|]

-- A ':' inside a string literal is not mistaken for the spec separator.
qq_fmt_spec_colon :: T.Text           -- -> "a:b"
qq_fmt_spec_colon = [fmt|{T.pack "a:b"}|]

-- {{ / }} doubling and the existing \{ escape, together.
qq_fmt_spec_escapes :: T.Text         -- -> "{x} and {y} done"
qq_fmt_spec_escapes = [fmt|{{x}} and \{y} done|]

-- K canary: a GADT whose sibling case alts call `show` at two refined types
-- (Int / Double).  Guards the DataConTable / stableVarId-collision class that
-- the CLAUDE.md GADT-sibling-alt note tracks.  Returns "1.5".
data FmtK a where
  FmtKInt  :: FmtK Int
  FmtKPrec :: Int -> FmtK Double

useFmtK :: FmtK a -> a -> T.Text
useFmtK k x = case k of
  FmtKInt    -> T.pack (show x)   -- show @Int
  FmtKPrec _ -> T.pack (show x)   -- show @Double

qq_fmt_usek :: T.Text
qq_fmt_usek = useFmtK (FmtKPrec 1) (1.5 :: Double)

-- ---- qq-suite: json section (owner: leaf qq-json) ----

-- One-liner sanity: a bare scalar.
qq_j_scalar :: Value
qq_j_scalar = [j|null|]

-- Nested object/array with string escapes, negative & fractional numbers,
-- bools and null; no antiquotes (pure constructor application).
qq_j_build :: Value
qq_j_build = [j|
  { "name": "tide\npool"
  , "count": -3
  , "ratio": 2.5
  , "items": [1, 2, 3]
  , "active": true
  , "missing": null
  , "nested": {"deep": [false, "x\u00e9"]}
  }
|]

-- Both antiquote forms over where-bound Int/Text values (exercises toJSON).
qq_j_anti :: Value
qq_j_anti = [j|{"id": $x, "upper": {T.toUpper name}}|]
  where
    x :: Int
    x = 7
    name :: T.Text
    name = T.pack "tide"

-- Pattern side: open-world object match binding one key, then unwrap.
qq_j_pat_extract :: T.Text
qq_j_pat_extract =
  case built of
    [j|{"name": $n}|] -> case n of
      String s -> s
      _        -> T.pack "?"
    _ -> T.pack "no-match"
  where
    built :: Value
    built = [j|{"name": "river", "size": 4}|]

-- Pattern side: literal leaves (number + string + null) with a failing first
-- arm that falls through to the matching arm.
qq_j_pat_literal :: Bool
qq_j_pat_literal =
  case [j|[1, "two", null]|] of
    [j|[1, "three", null]|] -> False
    [j|[1, "two", null]|]   -> True
    _                       -> False

-- Pattern side: fixed-prefix array match on a longer array; combine binders.
qq_j_pat_array :: Int
qq_j_pat_array =
  case [j|[10, 20, 30, 40]|] of
    [j|[$a, $b, ...]|] -> case (a, b) of
      (Number da, Number db) -> if da < db then 1 else 0
      _                      -> -1
    _ -> 0

-- Pattern side: nested object -> array -> element, open-world throughout.
qq_j_pat_nested :: T.Text
qq_j_pat_nested =
  case nested of
    [j|{"user": {"tags": [$first, ...]}}|] -> case first of
      String s -> s
      _        -> T.pack "?"
    _ -> T.pack "no-match"
  where
    nested :: Value
    nested = [j|{"user": {"tags": ["alpha", "beta"], "id": 1}}|]

-- Pattern side: open-world: one-key pattern matches a three-key object.
qq_j_pat_open :: Bool
qq_j_pat_open =
  case [j|{"a": 1, "b": 2, "c": 3}|] of
    [j|{"b": $v}|] -> case v of
      Number d -> d == 2.0
      _        -> False
    _ -> False
