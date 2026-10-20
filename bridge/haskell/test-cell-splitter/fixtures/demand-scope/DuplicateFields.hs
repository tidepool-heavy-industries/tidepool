{-# LANGUAGE GHC2024, DuplicateRecordFields #-}
module DuplicateFields where
import Prelude
data A = A { field :: Int }
data B = B { field :: Bool }
root = case A 1 of { A {field=old} -> old }
helper = case B True of { B {field=old} -> old }
