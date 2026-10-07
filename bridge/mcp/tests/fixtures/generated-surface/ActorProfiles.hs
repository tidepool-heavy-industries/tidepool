{-# LANGUAGE TypeApplications #-}
module Main where

import Prelude
import Tidepool.Actors.Role (ActorEffects)
import Tidepool.Effects.Row (effectKeys, knownEffects)

-- The row comes from GHC's dictionary for the ordinary default alias.
main :: IO ()
main = print
  [ "ActorEffects" : map show (effectKeys (knownEffects @ActorEffects))
  ]
