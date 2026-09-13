module Main where

import BignumContract qualified as Contract

main :: IO ()
main = do
  print Contract.pow2ToHundredText
  print Contract.factorial30Text
  print Contract.primeProductMod
  print Contract.gcdLargeText
  print Contract.negativeQuotRem
  print Contract.largeVersusSmall
