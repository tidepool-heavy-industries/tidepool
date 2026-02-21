module Suite where

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
