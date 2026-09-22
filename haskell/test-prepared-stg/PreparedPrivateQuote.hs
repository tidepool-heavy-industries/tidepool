{-# LANGUAGE TemplateHaskell #-}
module PreparedPrivateQuote (quoteAnswer) where

import Language.Haskell.TH.Quote (QuasiQuoter(..))
import PreparedPrivateOwner (answer)

quoteAnswer :: QuasiQuoter
quoteAnswer = QuasiQuoter
  { quoteExp = \_ -> [| answer |]
  , quotePat = \_ -> fail "expression only"
  , quoteType = \_ -> fail "expression only"
  , quoteDec = \_ -> fail "expression only"
  }
