{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

module RequestTypeDiagnostic where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Exomonad

{-# NOINLINE unresolved #-}
unresolved
  :: forall answer. AgentRef
  -> Eff ActorEffects (Either RequestError (Request answer))
unresolved agent = request @answer agent (7 :: Int) defaultRequestOptions

annotated
  :: AgentRef
  -> Eff ActorEffects (Either RequestError (Request Bool))
annotated agent = request @Bool agent (7 :: Int) defaultRequestOptions

functionAnswer
  :: AgentRef
  -> Eff ActorEffects (Either RequestError (Request (Int -> Int)))
functionAnswer agent = request @(Int -> Int) agent (7 :: Int) defaultRequestOptions
