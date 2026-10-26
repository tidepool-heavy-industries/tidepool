module CachedPackageWorldEntry where

import qualified CachedPackageWorldHome as Cached

__prepared :: Int
__prepared = Cached.cachedTextLength "cached source" + Cached.cachedIdentity 17
