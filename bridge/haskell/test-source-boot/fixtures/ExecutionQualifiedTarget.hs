{-# LANGUAGE QuasiQuotes #-}
module ExecutionQualifiedTarget where

import qualified ExecutionSealedQuoter as Q (answer)

__result :: Int
__result = [Q.answer|quoted|]
