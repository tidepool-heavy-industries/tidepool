module Library where

import Data.Maybe (mapMaybe)

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

-- ===========================================================================
-- § Classify & Group
-- ===========================================================================

-- | Group items by a classifier, preserving order of first occurrence.
classifyBy :: Eq k => (a -> k) -> [a] -> [(k, [a])]
classifyBy f = foldr insert []
  where
    insert x [] = [(f x, [x])]
    insert x ((k, vs):rest)
      | f x == k  = (k, x : vs) : rest
      | otherwise = (k, vs) : insert x rest

-- | Count items per group.
tally :: [(k, [a])] -> [(k, Int)]
tally = map (\(k, vs) -> (k, Prelude.length vs))

-- | Apply an effectful action to each group, collecting results.
batchMapM :: Monad m => (k -> [a] -> m [b]) -> [(k, [a])] -> m [(k, [b])]
batchMapM f = mapM (\(k, vs) -> do { rs <- f k vs; pure (k, rs) })

-- | Filter groups by a predicate on (key, items).
groupFilter :: (k -> [a] -> Bool) -> [(k, [a])] -> [(k, [a])]
groupFilter p = filter (\(k, vs) -> p k vs)

-- | Flatten grouped results back to a list.
flatten :: [(k, [a])] -> [a]
flatten = concatMap snd

-- | Transform group keys.
rekey :: (k -> j) -> [(k, a)] -> [(j, a)]
rekey f = map (\(k, v) -> (f k, v))

-- | Monadic pipeline over groups: classify, transform each group, flatten.
withGroups :: (Monad m, Eq k)
           => (a -> k) -> (k -> [a] -> m [b]) -> [a] -> m [b]
withGroups classify act xs =
  flatten <$> batchMapM act (classifyBy classify xs)

-- | Format groups as a text report. Render function gets (key, count).
report :: (k -> Int -> [Char]) -> [(k, [a])] -> [Char]
report fmt groups =
  Prelude.unlines (map (\(k, vs) -> fmt k (Prelude.length vs)) groups)

-- ===========================================================================
-- § Oracle Combinators (ask-steered effectful folds)
-- ===========================================================================

-- | Walk a list with monadic steering at each element.
--   The suspend function yields context and returns a response.
--   The step function uses the response to decide: Left = stop, Right = continue.
steer :: Monad m
      => (Int -> Int -> a -> m r)
      -> (r -> a -> b -> Either b b)
      -> b -> [a] -> m b
steer suspend step = go 0
  where
    go _ acc [] = pure acc
    go i acc (x:xs) = do
      resp <- suspend i (Prelude.length xs) x
      case step resp x acc of
        Left  b -> pure b
        Right b -> go (i+1) b xs

-- | Like steer but the step function is effectful.
steerM :: Monad m
       => (Int -> Int -> a -> m r)
       -> (r -> a -> b -> m (Either b b))
       -> b -> [a] -> m b
steerM suspend step = go 0
  where
    go _ acc [] = pure acc
    go i acc (x:xs) = do
      resp <- suspend i (Prelude.length xs) x
      r <- step resp x acc
      case r of
        Left  b -> pure b
        Right b -> go (i+1) b xs

-- | Oracle-steered unfold: suspend at each step, response steers next seed.
oracleAna :: Monad m
          => (r -> a -> m (Maybe (b, a)))
          -> (a -> m r)
          -> a -> m [b]
oracleAna step suspend seed = do
  resp <- suspend seed
  r <- step resp seed
  case r of
    Nothing      -> pure []
    Just (b, a') -> (b :) <$> oracleAna step suspend a'

-- | Oracle hylo: unfold with steering, then fold the results.
oracleHylo :: Monad m
           => (r -> a -> m (Maybe (b, a)))
           -> (a -> m r)
           -> (b -> c -> c)
           -> c -> a -> m c
oracleHylo step suspend alg z seed = do
  items <- oracleAna step suspend seed
  pure (foldr alg z items)

-- ===========================================================================
-- § Rose Trees
-- ===========================================================================

data Rose a = Rose a [Rose a]

roseVal :: Rose a -> a
roseVal (Rose x _) = x

roseChildren :: Rose a -> [Rose a]
roseChildren (Rose _ cs) = cs

foldRose :: (a -> [b] -> b) -> Rose a -> b
foldRose f (Rose x cs) = f x (map (foldRose f) cs)

mapRose :: (a -> b) -> Rose a -> Rose b
mapRose f (Rose x cs) = Rose (f x) (map (mapRose f) cs)

flattenRose :: Rose a -> [a]
flattenRose (Rose x cs) = x : concatMap flattenRose cs

roseDepth :: Rose a -> Int
roseDepth (Rose _ []) = 0
roseDepth (Rose _ cs) = 1 + foldl' (\acc c -> max acc (roseDepth c)) 0 cs

roseSize :: Rose a -> Int
roseSize = foldRose (\_ cs -> 1 + foldl' (+) 0 cs)

-- | All root-to-leaf paths
rosePaths :: Rose a -> [[a]]
rosePaths (Rose x []) = [[x]]
rosePaths (Rose x cs) = map (x :) (concatMap rosePaths cs)

-- | Filter subtrees by predicate on node value (prunes entire subtree)
roseFilter :: (a -> Bool) -> Rose a -> [Rose a]
roseFilter p (Rose x cs)
  | p x       = [Rose x (concatMap (roseFilter p) cs)]
  | otherwise = []

-- | Build a rose tree from a seed (anamorphism)
unfoldRose :: (a -> (b, [a])) -> a -> Rose b
unfoldRose f seed = let (b, seeds) = f seed in Rose b (map (unfoldRose f) seeds)

-- | Monadic fold over a rose tree
foldRoseM :: Monad m => (a -> [b] -> m b) -> Rose a -> m b
foldRoseM f (Rose x cs) = mapM (foldRoseM f) cs >>= f x

-- ===========================================================================
-- § Ask-Steered Traversals (pure combinators, parameterized over monad)
-- ===========================================================================

-- | Walk items with monadic steering at each step.
--   Present each item, fold responses into accumulator.
--   Step function returns Left to halt, Right to continue.
guidedFoldM :: Monad m
            => (Int -> a -> m r)    -- ^ present item with index
            -> (r -> a -> b -> Either b b)  -- ^ integrate response
            -> b -> [a] -> m b
guidedFoldM _ _ z [] = pure z
guidedFoldM present step z (x:xs) = do
  resp <- present (Prelude.length xs) x
  case step resp x z of
    Left  b -> pure b
    Right b -> guidedFoldM present step b xs

-- | Explore a rose tree with monadic guidance.
--   At each non-leaf node, present the node and its children.
--   The pick function selects which child indices to recurse into.
guidedSearchM :: Monad m
              => (a -> [a] -> Int -> m [Int])  -- ^ node, child vals, depth → selected indices
              -> Rose a -> m [a]
guidedSearchM pick = go 0
  where
    go d (Rose x []) = pure [x]
    go d (Rose x cs) = do
      indices <- pick x (map roseVal cs) d
      let selected = mapMaybe (\i -> cs !? i) indices
      results <- mapM (go (d+1)) selected
      pure (x : concat results)

-- | Iterative refinement: transform state, yield for feedback, repeat.
--   Stops when the check function returns Nothing (= done).
refineM :: Monad m
        => Int                     -- ^ max iterations
        -> (s -> m (Maybe s))      -- ^ present state, get adjusted state (Nothing = done)
        -> s -> m s
refineM 0 _ s = pure s
refineM n step s = do
  r <- step s
  case r of
    Nothing -> pure s
    Just s' -> refineM (n - 1) step s'

-- | Execute named phases with monadic review between each.
--   Review function can revise the input for the next phase.
phasesM :: Monad m
        => (Prelude.String -> a -> m a)   -- ^ review: phase name, result → next input
        -> [(Prelude.String, a -> m a)]   -- ^ named phases
        -> a -> m [a]
phasesM _ [] _ = pure []
phasesM review ((name, act):rest) input = do
  result <- act input
  nextInput <- review name result
  (result :) <$> phasesM review rest nextInput

-- | Map over rose tree children with monadic filter.
--   Useful for pruning subtrees based on effectful checks.
pruneRoseM :: Monad m => (a -> m Bool) -> Rose a -> m (Maybe (Rose a))
pruneRoseM p (Rose x cs) = do
  keep <- p x
  if keep
    then do
      cs' <- mapMaybeM (pruneRoseM p) cs
      pure (Just (Rose x cs'))
    else pure Nothing

mapMaybeM :: Monad m => (a -> m (Maybe b)) -> [a] -> m [b]
mapMaybeM _ [] = pure []
mapMaybeM f (x:xs) = do
  r <- f x
  rest <- mapMaybeM f xs
  case r of
    Just b  -> pure (b : rest)
    Nothing -> pure rest

-- mapMaybe: use the one from Tidepool.Prelude (re-exported from Data.Maybe)
