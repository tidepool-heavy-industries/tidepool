-- | Shared JSON string escaping for the extractor's hand-rolled JSON
-- renderers (see "Tidepool.Binders" and "Tidepool.DiagJson"). No @aeson@
-- dependency: the escaping need is small and fixed.
module Tidepool.Json
  ( jsonString
  ) where

import Data.Char (ord)
import Numeric (showHex)

-- | JSON string escaping for arbitrary text.
jsonString :: String -> String
jsonString s = '"' : concatMap esc s ++ "\""
  where
    esc '"'  = "\\\""
    esc '\\' = "\\\\"
    esc '\n' = "\\n"
    esc '\r' = "\\r"
    esc '\t' = "\\t"
    esc c
      | c < '\x20' = "\\u" ++ pad4 (showHex (ord c) "")
      | otherwise  = [c]
    pad4 s' = replicate (4 - length s') '0' ++ s'
