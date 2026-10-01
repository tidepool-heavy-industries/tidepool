{-# LANGUAGE QuasiQuotes #-}
module MetadataQuotedTarget where

import MetadataQuoter (answer)

__result :: Int
__result = [answer|quoted|]
