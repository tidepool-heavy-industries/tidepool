{-# LANGUAGE QuasiQuotes #-}
module ExecutionReexportTarget where

import ExecutionReexportFacade (answer)

__result :: Int
__result = [answer|retained reexport|]
