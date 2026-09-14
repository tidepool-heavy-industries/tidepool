{-# LANGUAGE MagicHash #-}
-- | GHC-native oracle for the S6 end-to-end retained-import test
-- (`tidepool-runtime/tests/prepared_execution.rs`). Prints
-- 'ImportConsumer.consumerValueAt' applied to @0#@ (the argument is unused)
-- and 'consumerResult', evaluated natively, so the values the Rust test
-- compares against come from the pinned GHC, never hand-derived. Run under
-- the pinned GHC:
--
--   cd haskell/test-prepared-stg && runghc -i. ImportConsumerOracle.hs
--
-- `ImportConsumerExpectations.json` is transcribed from this program's
-- stdout, per the `FreerResumeOracle.hs` precedent.
module Main where

import ImportConsumer (consumerResult, consumerValueAt)

main :: IO ()
main = do
  print (consumerValueAt 0#)
  print consumerResult
