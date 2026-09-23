module Main where

import TextContract qualified as Contract

main :: IO ()
main = do
  print Contract.splitOnParts
  print Contract.intercalated
  print Contract.replaced
  print Contract.upperWordsRoundTrip
  print Contract.nonAsciiLength
  print Contract.packedShow
  print Contract.charFold
  print Contract.textKeyedLookup
  print Contract.reversedGreek
  print Contract.splitGreekPath
