{-# LANGUAGE QuasiQuotes #-}
module ExecutionClassQuoteTarget where

import ExecutionClassQuoter (Quote(..))

__result :: Int
__result = [classAnswer| |]
