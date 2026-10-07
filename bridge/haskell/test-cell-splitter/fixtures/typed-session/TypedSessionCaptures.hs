{-# LANGUAGE DataKinds #-}
module TypedSessionCaptures where

import Control.Monad.Freer (Eff)
import Tidepool.Effects.Core ()
import qualified Tidepool.Internal.Resume as TidepoolResume

-- Internal compiler fixture: the resident cell parser still rejects local
-- fixities. The protected parsed-source owner inserts its own item markers.
__typedSessionRoot :: Eff '[] ()
__typedSessionRoot = do
  let { infixr 4 `minus`; minus :: Int -> Int -> Int; minus x y = x - y }
  let answer = 37 :: Int
  pure ()
