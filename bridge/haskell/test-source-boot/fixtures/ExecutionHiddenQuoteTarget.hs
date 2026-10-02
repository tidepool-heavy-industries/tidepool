{-# LANGUAGE QuasiQuotes #-}
module ExecutionHiddenQuoteTarget where

import qualified ExecutionSealedQuoter as Q hiding (answer)

__result :: Int
__result = [Q.answer|quoted|]
