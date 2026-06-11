-- | Phase-0 spike quoter: [upper|hello|] -> "HELLO".
-- Deliberately minimal: imports only base + template-haskell (package
-- modules), so the only HOME module needing splice-time code is this one.
module UpperQQ (upper) where

import Data.Char (toUpper)
import Language.Haskell.TH (litE, stringL)
import Language.Haskell.TH.Quote (QuasiQuoter (..))

upper :: QuasiQuoter
upper = QuasiQuoter
  { quoteExp  = \s -> litE (stringL (map toUpper s))
  , quotePat  = \_ -> fail "upper: pattern context unsupported"
  , quoteType = \_ -> fail "upper: type context unsupported"
  , quoteDec  = \_ -> fail "upper: declaration context unsupported"
  }
