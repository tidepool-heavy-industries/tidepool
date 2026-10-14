{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module Main where

import Prelude
import Data.Kind (Type)
import Data.Proxy (Proxy (..))
import Data.Text (unpack)
import Data.Typeable (Typeable, typeRep, typeRepTyCon, tyConModule, tyConName)
import Tidepool.Agent.ToolEffects (toolEffectNames)
import Tidepool.Effects.Row (effectKeys, knownEffects)
import Tidepool.Internal.ActorProfiles (ActorEffects, CatalogueEffects, CatalogueToolEffects, actorEffectName)

-- GHC owns defining TyCon identity; no generated module/name table is read here.
class Nominals (effects :: [Type -> Type]) where
  nominals :: Proxy effects -> [String]
instance Nominals '[] where
  nominals _ = []
instance (Typeable effect, Nominals effects) => Nominals (effect ': effects) where
  nominals _ =
    let ctor = typeRepTyCon (typeRep (Proxy @effect))
    in (tyConModule ctor ++ "." ++ tyConName ctor) : nominals (Proxy @effects)

main :: IO ()
main = print
  [ "ActorEffects" : map show (effectKeys (knownEffects @ActorEffects))
  , "CatalogueConstructors" : map show (effectKeys (knownEffects @CatalogueEffects))
  , "CatalogueEffects" : map (unpack . actorEffectName) (effectKeys (knownEffects @CatalogueEffects))
  , "Nominals" : nominals (Proxy @CatalogueEffects)
  , "ToolEffects" : map unpack (toolEffectNames (Proxy @CatalogueToolEffects))
  , "ToolNominals" : nominals (Proxy @CatalogueToolEffects)
  ]
