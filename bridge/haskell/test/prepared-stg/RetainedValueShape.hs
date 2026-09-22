module RetainedValueShape
  ( RetainedValue(..)
  , importedRetainedValue
  ) where

-- Shaped like a value imported from an earlier resident turn: it contains a
-- closure and a shared lifted environment rather than a source-level scalar.
data RetainedValue = RetainedValue
  { retainedApply :: Int -> Int
  , retainedEnvironment :: [Int]
  }

importedRetainedValue :: RetainedValue
importedRetainedValue =
  let shared = [10, 20, 30]
      offset x = case shared of
        first : _ -> x + first
        [] -> x
  in RetainedValue offset shared
