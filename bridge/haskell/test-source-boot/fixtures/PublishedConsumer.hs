module PublishedConsumer (answer) where

import PublishedSeed (seed)
import PublishedSibling (sibling)

answer :: Int
answer = seed + sibling
