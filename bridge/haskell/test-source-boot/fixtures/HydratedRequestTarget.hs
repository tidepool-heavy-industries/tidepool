{-# LANGUAGE TypeApplications #-}
module HydratedRequestTarget where
import Tidepool.Actors.Internal.Agent

result :: Maybe Bool
result = request @Bool @Char 'x'
