module Main where

import FingerprintExecutionContract qualified as Contract

main :: IO ()
main = do
  print Contract.emptyFingerprint
  print Contract.abcFingerprint
  print Contract.multiblockFingerprint
