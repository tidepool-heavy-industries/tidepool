module RejectCoerceScope where

import Data.Coerce (coerce)
import Sketch

-- Must fail even though scope has no runtime representation.
bad :: Selected first Steps -> Selected second Steps
bad = coerce
