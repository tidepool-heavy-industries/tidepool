{-# LANGUAGE TypeApplications #-}
module MarkerCensusExpr where
import Tidepool.Actor

__result :: (Bool, Int, Maybe Bool)
__result = (serve @Bool True, receive @Int (ordinary 16), Nothing)
