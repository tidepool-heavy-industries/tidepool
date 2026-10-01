module ExecutionHiddenOrphan () where

import ExecutionClass (C(..))

instance C Int where
  c _ = 42
