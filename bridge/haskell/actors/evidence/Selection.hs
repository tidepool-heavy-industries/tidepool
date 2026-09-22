-- Task-level evidence, not actor-runtime lifecycle or response state.
module Selection (Selection(..), hasExecutedSelection) where

data Selection = SelectedCounts { selectedCount :: Int, executedCount :: Int }
               | SelectionUnknown
  deriving (Eq, Show)

-- Unknown, empty, partial, and inconsistent execution cannot close a check.
hasExecutedSelection :: Selection -> Bool
hasExecutedSelection (SelectedCounts selected executed) =
  selected > 0 && executed == selected
hasExecutedSelection SelectionUnknown = False
