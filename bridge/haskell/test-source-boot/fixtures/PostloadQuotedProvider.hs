{-# LANGUAGE QuasiQuotes, TemplateHaskellQuotes #-}
module PostloadQuotedProvider (__result, answer) where

import Language.Haskell.TH (Exp(..), Lit(..))
import Language.Haskell.TH.Quote (QuasiQuoter(..))
import PostloadObservedQuoter qualified as Original

__result :: Int
__result = [Original.answer|{{QUOTER_COUNTER}}|]

-- The consumer must execute this module's code, not merely read its interface.
answer :: QuasiQuoter
answer = QuasiQuoter
  { quoteExp = \_ -> pure (LitE (IntegerL (toInteger __result)))
  , quotePat = \_ -> fail "expression only"
  , quoteType = \_ -> fail "expression only"
  , quoteDec = \_ -> fail "expression only"
  }
