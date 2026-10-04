module Main (main) where

import System.Environment (getArgs)
import QuasiQuoteOccurrencesBenchmark (quasiQuoteOccurrenceBenchmark)

main :: IO ()
main = getArgs >>= \arguments -> case arguments of
  iterations : files | not (null files) -> quasiQuoteOccurrenceBenchmark iterations files
  _ -> fail "usage: quasiquote-occurrences-benchmark ITERATIONS SOURCE..."
