{-# LANGUAGE TemplateHaskellQuotes #-}
module MetadataQuoter (answer) where

import Language.Haskell.TH (Exp(..), Lit(..))
import Language.Haskell.TH.Quote (QuasiQuoter(..))
import MetadataQuoteSupport (answerValue)

answer :: QuasiQuoter
answer = QuasiQuoter
  { quoteExp = \_ -> pure (LitE (IntegerL answerValue))
  , quotePat = \_ -> fail "expression only"
  , quoteType = \_ -> fail "expression only"
  , quoteDec = \_ -> fail "expression only"
  }
