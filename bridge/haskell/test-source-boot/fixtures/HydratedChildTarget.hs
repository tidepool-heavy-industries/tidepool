{-# LANGUAGE TypeApplications #-}
module HydratedChildTarget where
import Tidepool.Actors.Unfold

result :: Maybe Bool
result = child @Bool @Int @Char @String 'x'
