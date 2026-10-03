{-# LANGUAGE TemplateHaskell #-}
module FinalizedSpliceOwner (makeAnswer) where

import Language.Haskell.TH
import System.Environment (getEnv)

answer :: Int
answer = $(do
  path <- runIO (getEnv "TIDEPOOL_TEST_FINALIZATION_COUNTER")
  runIO (appendFile path "owner\n")
  [| 42 :: Int |])

makeAnswer :: Q Exp
makeAnswer = pure (LitE (IntegerL (fromIntegral answer)))
