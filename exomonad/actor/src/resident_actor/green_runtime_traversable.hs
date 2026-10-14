import qualified Data.Map.Strict as Map

_ <- do
  let inputs = Map.fromList [(17 :: Int, "one"), (42, "two")]
  answers <- Async.mapConcurrently
    (\name -> awaitGreenForm name (if name == "one"
      then \x -> x + (11 :: Int)
      else \x -> x * (7 :: Int))) inputs
  flipped <- Async.forConcurrently (Just (3 :: Int)) (\x -> pure (x + 1))
  ordered <- Async.forConcurrently [3 :: Int, 1, 2] (\x -> pure (x * 10))
  emptyMap <- Async.mapConcurrently (\_ -> Core.error "empty map spawned a thread")
    (Map.empty :: Map.Map Int Int)
  emptyList <- Async.forConcurrently ([] :: [Int])
    (\_ -> Core.error "empty list spawned a thread")
  say (tshow (Map.keys answers == [17, 42]
    && (case Map.lookup 17 answers of
      Just first -> first 5 == 16
      Nothing -> False)
    && (case Map.lookup 42 answers of
      Just second -> second 5 == 35
      Nothing -> False)
    && flipped == Just 4 && ordered == [30, 10, 20]
    && Map.null emptyMap && null emptyList))
