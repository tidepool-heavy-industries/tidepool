{-# LANGUAGE DataKinds #-}
module OriginalTextConsumer where

import Control.Monad.Freer (Eff, send)
import qualified Tidepool.Effects.Core as Core
import OriginalTextRequest (request)

result :: Eff '[Core.Forks] ()
result = send request >> pure ()
