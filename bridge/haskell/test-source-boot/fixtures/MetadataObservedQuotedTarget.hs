{-# LANGUAGE TemplateHaskell #-}
module MetadataObservedQuotedTarget where

import Language.Haskell.TH (Exp(..), Lit(..), runIO)
import Language.Haskell.TH.Quote (quoteExp)
import MetadataQuoter (answer)

__result :: Int
__result = $(do
  expression <- quoteExp answer "quoted"
  case expression of
    LitE (IntegerL value) -> do
      runIO (appendFile "{{QUOTER_COUNTER}}" (show value ++ "\n"))
      pure expression
    _ -> fail "metadata quoter did not produce its native integer expression")
