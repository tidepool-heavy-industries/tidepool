{-# LANGUAGE QuasiQuotes #-}
module PostloadProviderConsumer where

import PostloadQuotedProvider (answer)

__result :: Int
__result = [answer|consumer|]
