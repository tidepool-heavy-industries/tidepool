{-# LANGUAGE DataKinds, PartialTypeSignatures #-}
module ResidentBind where

import Control.Monad.Freer (Eff)
import Tidepool.Prelude
import qualified Tidepool.Internal.Resume as TidepoolResume

__result :: Eff '[] _
__result = do {
  {{TURN_STMT}}
 ; pure ({{BINDERS}})
}
__prepared = TidepoolResume.settle __result
