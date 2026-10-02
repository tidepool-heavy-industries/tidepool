{-# LANGUAGE TemplateHaskell #-}
module UnusedCompileTime (unused) where

import Language.Haskell.TH (runIO)
import System.Environment (getEnv)

unused :: Int
unused = $(do
  runIO (getEnv "TIDEPOOL_TEST_UNUSED_SPLICE_COUNTER" >>= \path -> appendFile path "splice\n")
  [| 99 :: Int |])
