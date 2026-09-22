{-# LANGUAGE DataKinds, PartialTypeSignatures #-}
module ResidentBind where

import Control.Monad.Freer (Eff)
import Tidepool.Prelude

__result :: Eff '[] _
__result = do {
  {{TURN_STMT}}
 ; pure ({{BINDERS}})
}
