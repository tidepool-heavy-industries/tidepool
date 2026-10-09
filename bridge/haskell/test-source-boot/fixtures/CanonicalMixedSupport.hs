module CanonicalMixedSupport (Answer) where

import CanonicalMixedA (A)
import CanonicalMixedB (B)
import CanonicalMixedZ (Z)

type Answer = (A, B, Z)
