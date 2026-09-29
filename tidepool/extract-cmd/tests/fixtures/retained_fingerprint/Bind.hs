{-# LANGUAGE DataKinds #-}
module RetainedProbeBind where

import Control.Monad.Freer (Eff)
import qualified Tidepool.Internal.Resume as Resume

__result :: Eff '[] Int
__result = do {
  {{TURN_STMT}}
 ; pure ({{BINDERS}})
}
__prepared = Resume.settle __result
