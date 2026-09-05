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
  let Right label = requestLabel "review"
  in requestWith @result actor (requestOptions label (7 :: Int))

annotated :: AgentRef -> Eff ActorEffects (Response Bool)
annotated actor =
  let Right label = requestLabel "review"
  in requestWith @Bool actor (requestOptions label (7 :: Int))

functionResult :: AgentRef -> Eff ActorEffects (Response (Int -> Int))
functionResult actor =
  let Right label = requestLabel "transform"
  in requestWith @(Int -> Int) actor (requestOptions label (7 :: Int))
