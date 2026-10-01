{-# LANGUAGE QuasiQuotes #-}
module ExecutionClassQuoteHidden where

import qualified ExecutionClassQuoter as Q hiding (Quote(..))

__result :: Int
__result = [Q.classAnswer| |]
