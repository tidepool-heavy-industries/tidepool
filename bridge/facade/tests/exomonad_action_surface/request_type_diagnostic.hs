{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
module RequestTypeDiagnostic where

import Prelude
import Control.Monad.Freer (Eff)
import Tidepool.Actors.Exomonad

{-# NOINLINE unresolved #-}
unresolved :: forall result. AgentRef -> Eff ActorEffects (Response result)
unresolved actor =
  let requestLabel = [label|review|]
  in request @result actor (assignment requestLabel (7 :: Int))

annotated :: AgentRef -> Eff ActorEffects (Response Bool)
annotated actor =
  let requestLabel = [label|review|]
  in request @Bool actor (assignment requestLabel (7 :: Int))

functionResult :: AgentRef -> Eff ActorEffects (Response (Int -> Int))
functionResult actor =
  let requestLabel = [label|transform|]
  in request @(Int -> Int) actor (assignment requestLabel (7 :: Int))
