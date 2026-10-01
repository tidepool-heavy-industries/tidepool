{-# LANGUAGE FlexibleInstances #-}
module ExecutionClassQuoter (Quote(..)) where

import Language.Haskell.TH (Exp(..), Lit(..))
import Language.Haskell.TH.Quote (QuasiQuoter(..))

class Quote a where
  classAnswer :: a

instance Quote QuasiQuoter where
  classAnswer = QuasiQuoter
    { quoteExp = \_ -> pure (LitE (IntegerL 43))
    , quotePat = \_ -> fail "expression only"
    , quoteType = \_ -> fail "expression only"
    , quoteDec = \_ -> fail "expression only"
    }
