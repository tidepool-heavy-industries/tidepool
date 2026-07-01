-- | Pure, Monad-polymorphic combinators: recursion schemes (the ana/cata/hylo
-- family), bounded iteration, rose trees, grouping. NO effect imports —
-- everything here works in any Monad, which is what makes it importable everywhere.
--
-- LAYERING (import direction is strict):
--   Schemes (pure generics)
--     -> verb modules (Explore/Dev/...: effectful vocabularies)
--       -> Library (re-export facade; auto-imported into evals)
-- Verb modules import Schemes, never Library (re-export cycle).
module Schemes where

import Data.Maybe (mapMaybe)

-- | Effectful loop: run the body until it returns Left (the result); Right is
-- the next seed. Terminates WITH an answer (unlike `ana`, whose `Nothing`
-- termination carries no final value).
loopM :: Monad m => (a -> m (Either r a)) -> a -> m r
loopM f = go
  where go a = f a >>= either pure go

-- ===========================================================================
-- § Recursion Schemes
-- ===========================================================================

hylo :: (b -> c -> c) -> c -> (a -> Maybe (b, a)) -> a -> c
hylo f z g seed = case g seed of
  Nothing      -> z
  Just (b, a') -> f b (hylo f z g a')

ana :: (a -> Maybe (b, a)) -> a -> [b]
ana f seed = case f seed of
  Nothing      -> []
  Just (b, a') -> b : ana f a'

cata :: (a -> b -> b) -> b -> [a] -> b
cata = foldr

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

-- ===========================================================================
-- § Bounded Iteration
-- ===========================================================================

iterateN :: Int -> (a -> a) -> a -> [a]
iterateN 0 f x = []
iterateN n f x = x : iterateN (n - 1) f (f x)

converge :: Eq a => (a -> a) -> a -> a
converge = convergeN 1000

convergeN :: Eq a => Int -> (a -> a) -> a -> a
convergeN n _ x | n <= 0 = x
convergeN n f x = let x' = f x in if x == x' then x else convergeN (n - 1) f x'

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

-- ===========================================================================
-- § Text Utilities
-- ===========================================================================

-- padLeft/padRight removed — use Tidepool.Text versions (Text, not [Char])

-- | Split a list into chunks of size n
chunksOf :: Int -> [a] -> [[a]]
chunksOf _ [] = []
chunksOf n xs = let (a, b) = Prelude.splitAt n xs in a : chunksOf n b

-- | Sliding window of size n over a list
windows :: Int -> [a] -> [[a]]
windows n xs
  | Prelude.length xs < n = []
  | otherwise = Prelude.take n xs : windows n (Prelude.drop 1 xs)

-- (`indexed` removed: it was a verbatim duplicate of the Prelude's
-- `zipWithIndex :: [a] -> [(Int, a)]`. Use that. Dropping it also frees the
-- name for Control.Lens's `indexed`, now re-exported wholesale by the Prelude.)

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
removeAt is = map snd . filter (\(i,_) -> not (elem i is)) . zip [0..]

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
