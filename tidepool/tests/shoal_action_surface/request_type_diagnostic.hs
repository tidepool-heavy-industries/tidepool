{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
module RequestTypeDiagnostic where

import Prelude
import Control.Monad.Freer (Eff)
import Tidepool.Actors.Shoal

{-# NOINLINE unresolved #-}
unresolved :: forall result. AgentRef -> Eff ActorEffects (Response result)
unresolved actor =
  let label = "review" :: Label
  in requestWith @result actor (assignment label (7 :: Int))

annotated :: AgentRef -> Eff ActorEffects (Response Bool)
annotated actor =
  let label = "review" :: Label
  in requestWith @Bool actor (assignment label (7 :: Int))

functionResult :: AgentRef -> Eff ActorEffects (Response (Int -> Int))
functionResult actor =
  let label = "transform" :: Label
  in requestWith @(Int -> Int) actor (assignment label (7 :: Int))
