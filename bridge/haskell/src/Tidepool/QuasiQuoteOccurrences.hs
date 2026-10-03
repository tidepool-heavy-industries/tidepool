-- | Parsed quasiquote syntax, before the renamer expands it.
module Tidepool.QuasiQuoteOccurrences (quasiQuoteOccurrences) where

import Data.Generics (everything, mkQ)
import GHC (GhcPs, HsUntypedSplice(..), ParsedSource, unLoc)
import GHC.Driver.Session (DynFlags, xopt)
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Types.Name.Reader (RdrName)

-- | Preserve parser traversal order and duplicates. A successful parse with
-- quasiquote syntax disabled cannot contain a quasiquote node. Enabled syntax
-- retains the full generic walk, including ordinary TH splice children.
quasiQuoteOccurrences :: DynFlags -> ParsedSource -> [RdrName]
quasiQuoteOccurrences flags
  | not (xopt LangExt.QuasiQuotes flags) = const []
  | otherwise = everything (++) (mkQ [] selected) . unLoc
  where
    selected :: HsUntypedSplice GhcPs -> [RdrName]
    selected (HsQuasiQuote _ name _) = [name]
    selected _ = []
