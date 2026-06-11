{-# LANGUAGE TemplateHaskellQuotes #-}
-- | The @[j|...|]@ quasi-quoter: JSON Value literals (expression side)
-- and JSON shape matching (pattern side).
--
-- SCAFFOLD STUB — implementation owned by the qq-json leaf.
module Tidepool.QQ.Json (j) where

import Language.Haskell.TH.Quote (QuasiQuoter (..))

-- | @[j| {"user": {"id": $uid}} |]@ — JSON literal with antiquotes as an
-- expression; JSON shape destructuring as a pattern.
j :: QuasiQuoter
j = QuasiQuoter
  { quoteExp  = \_ -> fail "j: not implemented yet (qq-suite scaffold)"
  , quotePat  = \_ -> fail "j: not implemented yet (qq-suite scaffold)"
  , quoteType = \_ -> fail "j: cannot be used in type position"
  , quoteDec  = \_ -> fail "j: cannot be used in declaration position"
  }
