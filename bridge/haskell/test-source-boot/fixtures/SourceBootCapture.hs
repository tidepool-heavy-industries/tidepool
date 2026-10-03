module SourceBootCapture where

import qualified CacheEntry
import qualified NativeScopeOwner

result :: (Bool, Int)
result = (CacheEntry.result, NativeScopeOwner.value)
