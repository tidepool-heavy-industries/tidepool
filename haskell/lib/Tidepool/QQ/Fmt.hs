{-# LANGUAGE TemplateHaskellQuotes #-}
-- | The @[fmt|...|]@ quasi-quoter: Text interpolation.
--
-- SCAFFOLD STUB — implementation owned by the qq-fmt leaf.
module Tidepool.QQ.Fmt (fmt) where

import Language.Haskell.TH.Quote (QuasiQuoter (..))

-- | @[fmt|{name} has {show hp}hp|]@ — literal text with @{antiquote}@
-- holes, expanding to a @Data.Text@ concatenation chain.
fmt :: QuasiQuoter
fmt = QuasiQuoter
  { quoteExp  = \_ -> fail "fmt: not implemented yet (qq-suite scaffold)"
  , quotePat  = \_ -> fail "fmt: cannot be used in pattern position"
  , quoteType = \_ -> fail "fmt: cannot be used in type position"
  , quoteDec  = \_ -> fail "fmt: cannot be used in declaration position"
  }
