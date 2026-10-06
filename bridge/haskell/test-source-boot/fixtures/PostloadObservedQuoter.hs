{-# LANGUAGE TemplateHaskellQuotes #-}
module PostloadObservedQuoter (answer) where

import Language.Haskell.TH (Exp(..), Lit(..), runIO)
import Language.Haskell.TH.Quote (QuasiQuoter(..))
import MetadataQuoteSupport (answerValue)

answer :: QuasiQuoter
answer = QuasiQuoter
  { quoteExp = \counter -> do
      runIO (appendFile counter (show answerValue ++ "\n"))
      pure (LitE (IntegerL answerValue))
  , quotePat = \_ -> fail "expression only"
  , quoteType = \_ -> fail "expression only"
  , quoteDec = \_ -> fail "expression only"
  }
