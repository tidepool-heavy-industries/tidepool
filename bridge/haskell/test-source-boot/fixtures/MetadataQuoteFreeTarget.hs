{-# LANGUAGE QuasiQuotes #-}
module MetadataQuoteFreeTarget where

import MetadataQuoter (answer)

-- Neither [answer|quoted|] here nor in the string executes a provider.
quoteText :: String
quoteText = "[answer|quoted|]"

__result :: Int
__result = 0
