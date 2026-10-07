{-# LANGUAGE TypeApplications #-}
module Main where

import Prelude
import Tidepool.Actors.Role
  ( CoreEffects
  , ResearchLeafEffects
  , ResearchEffects
  , CodingEffects
  , IntegrationEffects
  , ActorEffects
  )
import Tidepool.Effects.Row (effectKeys, knownEffects)

-- String lists use the same array syntax as JSON for these constructor names.
-- The rows themselves come from GHC's dictionaries for the public aliases.
main :: IO ()
main = print
  [ "CoreEffects" : map show (effectKeys (knownEffects @CoreEffects))
  , "ResearchLeafEffects" : map show (effectKeys (knownEffects @ResearchLeafEffects))
  , "ResearchEffects" : map show (effectKeys (knownEffects @ResearchEffects))
  , "CodingEffects" : map show (effectKeys (knownEffects @CodingEffects))
  , "IntegrationEffects" : map show (effectKeys (knownEffects @IntegrationEffects))
  , "ActorEffects" : map show (effectKeys (knownEffects @ActorEffects))
  ]
