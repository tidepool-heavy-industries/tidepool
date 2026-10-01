{-# LANGUAGE QuasiQuotes #-}
module ExecutionFreshTarget where

import ExecutionFreshQuoter (answer)

__result :: Int
__result = [answer|quoted|]
