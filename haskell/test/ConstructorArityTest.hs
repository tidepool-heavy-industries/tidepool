module Main (main) where

import Control.Monad (unless)
import GHC.Builtin.Types
  ( eqDataCon, heqDataCon, coercibleDataCon, consDataCon, nilDataCon
  , intDataCon, tupleDataCon )
import Language.Haskell.Syntax.Basic (Boxity (..))
import Tidepool.Translate (valueRepArity)

main :: IO ()
main = mapM_ check
  [ ("homogeneous equality", eqDataCon, 0)
  , ("heterogeneous equality", heqDataCon, 0)
  , ("representational equality", coercibleDataCon, 0)
  , ("list cell", consDataCon, 2)
  , ("empty list", nilDataCon, 0)
  , ("boxed integer", intDataCon, 1)
  , ("boxed pair", tupleDataCon Boxed 2, 2)
  , ("unboxed pair", tupleDataCon Unboxed 2, 2)
  ]
  where
    check (label, constructor, expected) =
      unless (valueRepArity constructor == expected) $
        fail (label ++ ": erased constructor arity mismatch")
