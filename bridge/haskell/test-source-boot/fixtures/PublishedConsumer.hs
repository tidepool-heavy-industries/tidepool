module PublishedConsumer (answer, __prepared) where

import PublishedSeed (seed)
import PublishedSibling (sibling)

answer :: Int
answer = seed + sibling

__prepared :: Int
__prepared = answer
