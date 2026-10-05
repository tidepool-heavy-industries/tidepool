{-# LANGUAGE TemplateHaskellQuotes #-}
module ExecutionExplicitOrphanQuoter (answer) where

import ExecutionClass (C(..))
import ExecutionHiddenOrphan ()
import Language.Haskell.TH (Exp(..), Lit(..))
import Language.Haskell.TH.Quote (QuasiQuoter(..))

answer :: QuasiQuoter
answer = QuasiQuoter
  { quoteExp = \_ -> do
      let value = c (0 :: Int)
      if value == 42
        then pure (LitE (IntegerL value))
        else fail ("orphan instance returned " ++ show value ++ "; expected 42")
  , quotePat = \_ -> fail "expression only"
  , quoteType = \_ -> fail "expression only"
  , quoteDec = \_ -> fail "expression only"
  }
