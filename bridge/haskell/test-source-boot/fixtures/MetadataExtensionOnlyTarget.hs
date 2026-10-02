{-# LANGUAGE QuasiQuotes #-}
module MetadataExtensionOnlyTarget where

import MetadataOwner

-- A quote in a comment or string does not execute a quoter: [answer|quoted|]
quoteText :: String
quoteText = "[answer|quoted|]"

__result :: Int
__result = available (42 :: Int)
