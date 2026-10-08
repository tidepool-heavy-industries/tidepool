{-# LANGUAGE TypeApplications #-}
module HydratedSiteExpr where

import Tidepool.Actors.Internal.Agent (request)

{-# OPAQUE __result #-}
__result :: Char -> Maybe Bool
__result = request @Bool @Char

result :: Maybe Bool
result = __result 'r'
