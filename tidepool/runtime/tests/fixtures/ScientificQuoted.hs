{-# LANGUAGE QuasiQuotes #-}
module ScientificQuoted where

import Tidepool.Aeson.Value (Value)
import Tidepool.QQ (j)

result :: Value
result = [j|42|]
