module CheckedOriginalReplayProbe (result, __prepared) where

import qualified CheckedOriginalReplayOwner as Original

result :: Int
result = Original.value

__prepared :: Int
__prepared = result
