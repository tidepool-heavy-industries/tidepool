module Main (main) where

import System.Environment (getArgs)
import DeclarationJoinCases (recoveredConsumer, recoveredNextConsumer)

main :: IO ()
main = getArgs >>= \arguments -> case arguments of
  ["--consumer", root] -> recoveredConsumer root
  ["--next-consumer", root] -> recoveredNextConsumer root
  _ -> fail "declaration consumer requires role and declared scratch directory"
