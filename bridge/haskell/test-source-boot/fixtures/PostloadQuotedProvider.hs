{-# LANGUAGE QuasiQuotes #-}
module PostloadQuotedProvider where

import PostloadObservedQuoter (answer)

__result :: Int
__result = [answer|{{QUOTER_COUNTER}}|]
