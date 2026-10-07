pair@(left, right) <- pure ((21 :: Int), (22 :: Int))
~(lazyLeft, lazyRight) <- pure pair
Just refutable <- pure (Just (23 :: Int))
(_, !strictValue) <- pure ((24 :: Int), (25 :: Int))
answer <- pure (left + right + lazyLeft + lazyRight + refutable + strictValue)
