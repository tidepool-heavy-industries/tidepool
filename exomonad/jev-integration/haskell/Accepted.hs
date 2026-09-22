module Main where

import Sketch
import qualified Data.Map.Strict as Map

request :: Walk Questions
request = Walk
  (Steps (FollowInfo "supervise" ["terminate"], Edge 7)
         ("Enough evidence found", Evidence "termination path"))
  Map.empty

answer :: Walk Answers
answer = Walk exampleResult Map.empty

main :: IO ()
main = do
  let chosen = matchChoice (step answer) $ Steps
        (\(Edge edge) -> "follow " ++ show edge)
        (\(Evidence evidence) -> "finish " ++ evidence)
      probability = withChoice (step answer) probabilityOf
  if chosen == "follow 7" && probability == 0.8 && Map.null (children request)
    then putStrLn "record modes, heterogeneous handlers, and scoped result access passed"
    else error "sketch result mismatch"
