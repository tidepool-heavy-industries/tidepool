module Main where

import UserTypesContract qualified as Contract

main :: IO ()
main = do
  print Contract.defaultMethodPair
  print Contract.updatedOrderView
  print Contract.functorFoldSum
  print Contract.maybeChain
  print Contract.eitherTraversed
  print Contract.eitherTraverseFailure
  print Contract.evalExpr
  print Contract.prettyExpr
