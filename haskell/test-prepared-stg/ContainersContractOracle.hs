module Main where

import ContainersContract qualified as Contract

main :: IO ()
main = do
  print Contract.mapToAscList
  print Contract.insertWithTotal
  print Contract.adjustedLookup
  print Contract.alteredSize
  print Contract.foldedKeys
  print Contract.unionTotals
  print Contract.setMembership
  print Contract.setDifferenceList
