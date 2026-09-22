{-# LANGUAGE TypeApplications #-}

module MalformedPreparedSite where

import Tidepool.Actors.Unfold (child)

malformedSite :: String -> Maybe Bool
malformedSite = child @Bool @Int @Char @String
