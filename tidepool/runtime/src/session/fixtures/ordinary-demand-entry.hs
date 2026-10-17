{-# LANGUAGE DataKinds #-}
module OrdinaryDemandEntry where
import Control.Monad.Freer (Eff)
import OrdinaryDemandProbe (entry)
-- tidepool-preamble-imports-v1

__result :: Eff '[] Int
__result = pure entry
