-- | Parsed quasiquote syntax, before the renamer expands it.
module Tidepool.QuasiQuoteOccurrences (quasiQuoteOccurrences) where

import Data.Data (Data, cast, gmapQ)
import Data.Maybe (isJust)
import GHC (GhcPs, HsUntypedSplice(..), ParsedSource, unLoc)
import GHC.Data.FastString (FastString)
import GHC.Types.Name.Reader (RdrName)
import GHC.Types.SrcLoc (RealSrcSpan, SrcSpan)

-- | Preserve parser traversal order and duplicates. Literal text and source
-- locations cannot contain parsed splice nodes; a quasiquote's body is also
-- text, not a nested parsed Haskell expression. Ordinary TH splice bodies
-- remain traversable, so their nested quasiquotes are still discovered.
quasiQuoteOccurrences :: ParsedSource -> [RdrName]
quasiQuoteOccurrences = collect . unLoc
  where
    collect :: Data value => value -> [RdrName]
    collect value = case cast value :: Maybe (HsUntypedSplice GhcPs) of
      Just (HsQuasiQuote _ name _) -> [name]
      _ | opaque value -> []
        | otherwise -> concat (gmapQ collect value)

    opaque :: Data value => value -> Bool
    opaque value = isJust (cast value :: Maybe String)
      || isJust (cast value :: Maybe FastString)
      || isJust (cast value :: Maybe SrcSpan)
      || isJust (cast value :: Maybe RealSrcSpan)
