module ForgedEffectWitness where

import Data.Coerce (coerce)
import Tidepool.Effects.Core (Commands, Journal)
import Tidepool.Effects.Row (EffectWitness)

forge :: EffectWitness Commands -> EffectWitness Journal
forge = coerce
