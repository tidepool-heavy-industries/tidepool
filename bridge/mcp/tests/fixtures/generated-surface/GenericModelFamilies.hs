{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module Main where

import Data.Proxy (Proxy (..))
import Tidepool.Effects.Core
import Tidepool.Effects.Row

-- The signature must need only the published family dictionary, as reusable
-- hosted recipes do when constructing an ActorSpec's effect row.
genericKeys :: forall modelFamily. KnownFamily modelFamily => Proxy modelFamily -> [ActorEffectKey]
genericKeys _ = effectKeys (knownEffects @'[ModelCall modelFamily, ForkAgent modelFamily, SpawnAgent modelFamily])

main :: IO ()
main =
  if genericKeys (Proxy @OpenAI) == [EffectModelCallOpenAI, EffectForkAgentOpenAI, EffectSpawnAgentOpenAI]
      && genericKeys (Proxy @Argon) == [EffectModelCallArgon, EffectForkAgentArgon, EffectSpawnAgentArgon]
      && genericKeys (Proxy @DeepSeek) == [EffectModelCallDeepSeek, EffectForkAgentDeepSeek, EffectSpawnAgentDeepSeek]
    then pure ()
    else fail "generic family witnesses must preserve exact operation/family keys"
