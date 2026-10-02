{-# LANGUAGE TypeApplications #-}
module HydratedSiteExpr where

import Tidepool.Actors.Unfold (child)

{-# OPAQUE __result #-}
__result :: Char -> Maybe Bool
__result = child @Bool @Int @Char @String

result :: Maybe Bool
result = __result 'r'
