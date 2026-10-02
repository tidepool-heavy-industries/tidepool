{-# LANGUAGE QuasiQuotes #-}
module ExecutionSealedTarget where

import ExecutionSealedQuoter (answer)

__result :: Int
__result = [answer|quoted|]
