{-# LANGUAGE GADTs, ScopedTypeVariables #-}
__LANGUAGE__
module SegmentOracle where

import Data.IORef

default (Int, Double)

__DECLARATIONS__

main :: IO ()
main = do
  observations <- newIORef ([] :: [Int])
  let record value = modifyIORef' observations (++ [value])
__BODY__
  readIORef observations >>= print
