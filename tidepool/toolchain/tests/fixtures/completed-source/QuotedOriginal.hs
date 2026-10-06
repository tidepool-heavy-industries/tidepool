{-# LANGUAGE QuasiQuotes #-}
module QuotedOriginal (value) where

import QuotedProvider (capture)

{-# NOINLINE value #-}
value :: Int
value = [capture|QUOTE_INPUT_PATH|]
