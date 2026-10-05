{-# LANGUAGE QuasiQuotes #-}
module ExecutionExplicitOrphanTarget where

import qualified ExecutionSealedQuoter as Sealed (answer)
import ExecutionExplicitOrphanQuoter (answer)

retained :: Int
retained = [Sealed.answer|retained|]

__result :: Int
__result = [answer|fresh|]
