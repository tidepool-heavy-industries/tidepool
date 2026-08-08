-- | Coercion evidence is erased in Haskell, symmetrically with type evidence:
-- a 'CoVar' binder emits no runtime lambda and consumes no join-point
-- parameter slot, matching the applications that already drop 'Coercion'
-- arguments.
module Fidelity.Erasure (checks) where

import Fidelity.Harness (Check)

checks :: IO [Check]
checks = pure []
