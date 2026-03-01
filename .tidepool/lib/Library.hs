module Library where

-- ===========================================================================
-- § Recursion Schemes
-- ===========================================================================

hylo :: (b -> c -> c) -> c -> (a -> Maybe (b, a)) -> a -> c
hylo f z g seed = case g seed of
  Nothing      -> z
  Just (b, a') -> f b (hylo f z g a')

para :: (a -> [a] -> b -> b) -> b -> [a] -> b
para f z []     = z
para f z (x:xs) = f x xs (para f z xs)

ana :: (a -> Maybe (b, a)) -> a -> [b]
ana f seed = case f seed of
  Nothing      -> []
  Just (b, a') -> b : ana f a'

cata :: (a -> b -> b) -> b -> [a] -> b
cata = foldr

apo :: (a -> Either [b] (b, a)) -> a -> [b]
apo f seed = case f seed of
  Left bs      -> bs
  Right (b, a') -> b : apo f a'

treeHylo :: (c -> b -> c -> c) -> (a -> c) -> (a -> Either a (a, b, a)) -> a -> c
treeHylo alg leaf coalg seed = case coalg seed of
  Left a           -> leaf a
  Right (l, b, r)  -> alg (treeHylo alg leaf coalg l) b (treeHylo alg leaf coalg r)

zygo :: (a -> b -> b) -> (a -> (b, c) -> c) -> b -> c -> [a] -> c
zygo f g bz cz []     = cz
zygo f g bz cz (x:xs) =
  let b = f x (cata f bz xs)
      c = zygo f g bz cz xs
  in  g x (b, c)

-- Monadic variants
hyloM :: Monad m => (b -> c -> m c) -> c -> (a -> m (Maybe (b, a))) -> a -> m c
hyloM f z g seed = do
  r <- g seed
  case r of
    Nothing      -> pure z
    Just (b, a') -> hyloM f z g a' >>= f b

anaM :: Monad m => (a -> m (Maybe (b, a))) -> a -> m [b]
anaM f seed = do
  r <- f seed
  case r of
    Nothing      -> pure []
    Just (b, a') -> (b :) <$> anaM f a'

apoM :: Monad m => (a -> m (Either [b] (b, a))) -> a -> m [b]
apoM f seed = do
  r <- f seed
  case r of
    Left bs       -> pure bs
    Right (b, a') -> (b :) <$> apoM f a'

paraM :: Monad m => (a -> [a] -> b -> m b) -> b -> [a] -> m b
paraM f z []     = pure z
paraM f z (x:xs) = paraM f z xs >>= f x xs

-- ===========================================================================
-- § Lenses (van Laarhoven — compose with ^?, .~, %~, &)
-- ===========================================================================

lens :: (s -> a) -> (s -> a -> s) -> Functor f => (a -> f a) -> s -> f s
lens get set f s = (\a -> set s a) <$> f (get s)

_1 :: Functor f => (a -> f a) -> (a, b) -> f (a, b)
_1 f (a, b) = (\a' -> (a', b)) <$> f a

_2 :: Functor f => (b -> f b) -> (a, b) -> f (a, b)
_2 f (a, b) = (\b' -> (a, b')) <$> f b

ix :: Int -> Functor f => (a -> f a) -> [a] -> f [a]
ix i f xs = case splitAt i xs of
  (before, x:after) -> (\x' -> before ++ [x'] ++ after) <$> f x
  _ -> case xs of
    (x:_) -> (\_ -> xs) <$> f x
    []    -> (\_ -> xs) <$> f (error "ix: empty list")

-- ===========================================================================
-- § Bounded Iteration
-- ===========================================================================

iterateN :: Int -> (a -> a) -> a -> [a]
iterateN 0 f x = []
iterateN n f x = x : iterateN (n - 1) f (f x)

converge :: Eq a => (a -> a) -> a -> a
converge f x = let x' = f x in if x == x' then x else converge f x'

scanl' :: (b -> a -> b) -> b -> [a] -> [b]
scanl' f z []     = [z]
scanl' f z (x:xs) = z : scanl' f (f z x) xs

scanlM :: Monad m => (b -> a -> m b) -> b -> [a] -> m [b]
scanlM f z []     = pure [z]
scanlM f z (x:xs) = do
  z' <- f z x
  rest <- scanlM f z' xs
  pure (z : rest)

iterateWhile :: (a -> Bool) -> (a -> a) -> a -> [a]
iterateWhile p f x = if p x then x : iterateWhile p f (f x) else []

iterateWhileM :: Monad m => (a -> Bool) -> (a -> m a) -> a -> m [a]
iterateWhileM p f x = if p x
  then do { x' <- f x; rest <- iterateWhileM p f x'; pure (x : rest) }
  else pure []

until' :: (a -> Bool) -> (a -> a) -> a -> a
until' p f x = if p x then x else until' p f (f x)

untilM :: Monad m => (a -> Bool) -> (a -> m a) -> a -> m a
untilM p f x = if p x then pure x else f x >>= untilM p f

-- ===========================================================================
-- § Effect Orchestration (polymorphic over any Monad m)
-- ===========================================================================

-- | Retry an action up to n times, returning the first success
retry :: Monad m => Int -> m (Maybe a) -> m (Maybe a)
retry 0 act = pure Nothing
retry n act = do
  r <- act
  case r of
    Just x  -> pure (Just x)
    Nothing -> retry (n - 1) act

-- | Bracket: acquire, use, release (no exceptions — sequential guarantee)
bracket :: Monad m => m a -> (a -> m ()) -> (a -> m b) -> m b
bracket acquire release use = do
  a <- acquire
  b <- use a
  release a
  pure b

-- | Fold with early exit: step returns Left to bail, Right to continue
foldEarlyM :: Monad m => (b -> a -> m (Either b b)) -> b -> [a] -> m b
foldEarlyM step z [] = pure z
foldEarlyM step z (x:xs) = do
  r <- step z x
  case r of
    Left  b -> pure b
    Right b -> foldEarlyM step b xs

-- | Chain effectful transformations left-to-right
pipeline :: Monad m => [a -> m a] -> a -> m a
pipeline []     x = pure x
pipeline (f:fs) x = f x >>= pipeline fs

-- | Run the same input through multiple effectful functions
fanOut :: Monad m => [a -> m b] -> a -> m [b]
fanOut fs a = mapM (\f -> f a) fs

-- | Effectful unfold with side-channel emission at each step
tracedUnfoldM :: Monad m => (b -> m ()) -> (a -> m (Maybe (b, a))) -> a -> m [b]
tracedUnfoldM emit step seed = do
  r <- step seed
  case r of
    Nothing      -> pure []
    Just (b, a') -> do
      emit b
      rest <- tracedUnfoldM emit step a'
      pure (b : rest)

-- ===========================================================================
-- § Text Utilities
-- ===========================================================================

-- | Pad a string to a given width with spaces on the right
padRight :: Int -> [Char] -> [Char]
padRight n s = s ++ Prelude.replicate (max 0 (n - Prelude.length s)) ' '

-- | Pad a string to a given width with spaces on the left
padLeft :: Int -> [Char] -> [Char]
padLeft n s = Prelude.replicate (max 0 (n - Prelude.length s)) ' ' ++ s

-- | Split a list into chunks of size n
chunksOf :: Int -> [a] -> [[a]]
chunksOf _ [] = []
chunksOf n xs = let (a, b) = Prelude.splitAt n xs in a : chunksOf n b

-- | Sliding window of size n over a list
windows :: Int -> [a] -> [[a]]
windows n xs
  | Prelude.length xs < n = []
  | otherwise = Prelude.take n xs : windows n (Prelude.drop 1 xs)

-- | Zip a list with indices starting from 0
indexed :: [a] -> [(Int, a)]
indexed = go 0
  where
    go _ []     = []
    go !n (x:xs) = (n, x) : go (n + 1) xs

-- | Safe list indexing
(!?) :: [a] -> Int -> Maybe a
[] !? _ = Nothing
(x:_)  !? 0 = Just x
(_:xs) !? n = if n < 0 then Nothing else xs !? (n - 1)
infixl 9 !?

-- | Group consecutive equal elements and count them
-- e.g., histogram [1,1,2,3,3,3] = [(1,2),(2,1),(3,3)]
histogram :: Eq a => [a] -> [(a, Int)]
histogram [] = []
histogram (x:xs) =
  let (same, rest) = Prelude.span (== x) xs
  in  (x, 1 + Prelude.length same) : histogram rest

-- | Remove elements at specified indices
removeAt :: [Int] -> [a] -> [a]
removeAt is = map snd . filter (\(i,_) -> not (elem i is)) . indexed

-- | Insert an element at a given index
insertAt :: Int -> a -> [a] -> [a]
insertAt 0 y xs = y : xs
insertAt _ y [] = [y]
insertAt n y (x:xs) = x : insertAt (n - 1) y xs
