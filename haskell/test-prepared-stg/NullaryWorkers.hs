module NullaryWorkers where

import Prelude

-- Keep each worker reference in the prepared STG, including two references to
-- the same list constructor so projection must intern one object.
nullaryList :: [Int]
nullaryList = []
{-# NOINLINE nullaryList #-}

nullaryListAgain :: [Int]
nullaryListAgain = []
{-# NOINLINE nullaryListAgain #-}

nullaryBool :: Bool
nullaryBool = True
{-# NOINLINE nullaryBool #-}

nullaryMaybe :: Maybe Int
nullaryMaybe = Nothing
{-# NOINLINE nullaryMaybe #-}

-- Keep a first-class, undersaturated cons worker so it remains an imported
-- executable value rather than being lowered as a constructor application.
nonNullaryWorker :: Int -> [Int] -> [Int]
nonNullaryWorker = (:)
{-# NOINLINE nonNullaryWorker #-}

result :: ([Int], [Int], Bool, Maybe Int, Int -> [Int] -> [Int])
result =
  (nullaryList, nullaryListAgain, nullaryBool, nullaryMaybe
  , nonNullaryWorker)
{-# NOINLINE result #-}
