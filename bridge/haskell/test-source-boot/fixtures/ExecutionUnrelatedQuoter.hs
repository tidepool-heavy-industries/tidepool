{-# LANGUAGE TemplateHaskellQuotes #-}
module ExecutionUnrelatedQuoter (answer) where

import ExecutionClass (C(..))
import Language.Haskell.TH (Exp(..), Lit(..))
import Language.Haskell.TH.Quote (QuasiQuoter(..))

answer :: QuasiQuoter
answer = QuasiQuoter
  { quoteExp = \_ -> pure (LitE (IntegerL (c (0 :: Int))))
  , quotePat = \_ -> fail "expression only"
  , quoteType = \_ -> fail "expression only"
  , quoteDec = \_ -> fail "expression only"
  }
