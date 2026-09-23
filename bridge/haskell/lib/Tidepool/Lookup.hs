-- | Structured read-only lookup. This effect never invokes Jev; the workspace
-- tool composes it with its own selection and presentation policy.
module Tidepool.Lookup
  ( Lookup, LookupRequest (..), LookupBatch (..), LookupResult (..),
    LookupOutcome (..), LookupEntry (..), LookupKind (..),
    LookupAvailability (..), LookupOrigin (..), LookupQuality (..),
    LookupCandidate (..), LookupReference (..), LookupNamespace (..),
    lookupRaw, lookupRequest,
  ) where

import Data.Text (Text)
import Tidepool.Effects (lookupRaw)
import Tidepool.Effects.Core

-- | Build a raw request with the same bounded discovery defaults as the
-- hosted lookup tool.
lookupRequest :: [Text] -> LookupRequest
lookupRequest queries' = LookupRequest queries' True Nothing 128 []
