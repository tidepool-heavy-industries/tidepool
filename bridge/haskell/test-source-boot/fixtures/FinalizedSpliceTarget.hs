{-# LANGUAGE TemplateHaskell #-}
module FinalizedSpliceTarget (result) where

import FinalizedSpliceOwner (makeAnswer)
import FinalizedSpliceUnused ()
import Language.Haskell.TH (runIO)
import System.Environment (getEnv)

result :: Int
result = $(do
  path <- runIO (getEnv "TIDEPOOL_TEST_FINALIZATION_COUNTER")
  runIO (appendFile path "target\n")
  makeAnswer)

warned :: Maybe Int -> Int
warned (Just value) = value
