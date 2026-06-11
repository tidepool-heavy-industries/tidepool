{-# LANGUAGE TemplateHaskellQuotes #-}
-- | Spike quoter #2: uses [| |] quotes (hygienic NameG references) and
-- references a HOME module (SpikeHelper.shout) plus a package module
-- (Data.Text.pack) from inside the quote. Tests whether NameG names
-- crossing the splice boundary resolve in the eval pipeline.
module UpperTH (upperTH) where

import Language.Haskell.TH (litE, stringL)
import Language.Haskell.TH.Quote (QuasiQuoter (..))
import qualified Data.Text as T
import SpikeHelper (shout)

upperTH :: QuasiQuoter
upperTH = QuasiQuoter
  { quoteExp  = \s -> [| shout (T.pack $(litE (stringL s))) |]
  , quotePat  = \_ -> fail "upperTH: pattern context unsupported"
  , quoteType = \_ -> fail "upperTH: type context unsupported"
  , quoteDec  = \_ -> fail "upperTH: declaration context unsupported"
  }
