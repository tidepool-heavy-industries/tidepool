{-# LANGUAGE QuasiQuotes #-}
module PreparedPrivateClient where

import PreparedPrivateQuote (quoteAnswer)

result :: Int -> Int
result value = [quoteAnswer||] value
