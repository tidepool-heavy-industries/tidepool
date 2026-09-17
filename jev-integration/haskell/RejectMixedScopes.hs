module RejectMixedScopes where

import Sketch

-- Must fail: nested result elimination introduces distinct rigid scopes.
bad :: ChoiceResult Steps -> ChoiceResult Steps -> Double
bad first second = withChoice first $ \selected _ ->
  withChoice second $ \_ distribution -> probabilityOf selected distribution
