-- | Structured read-only lookup. This effect never invokes Jev; the workspace
-- tool composes it with its own selection and presentation policy.
module Tidepool.Lookup
  ( Lookup, LookupRequest (..), LookupBatch (..), LookupResult (..),
    LookupOutcome (..), LookupEntry (..), LookupKind (..),
    LookupAvailability (..), LookupOrigin (..), LookupQuality (..),
    LookupCandidate (..), LookupReference (..), LookupNamespace (..),
    lookupRaw,
  ) where

import Tidepool.Effects (lookupRaw)
import Tidepool.Effects.Core
