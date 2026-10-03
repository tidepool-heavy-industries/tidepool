{-# LANGUAGE ConstraintKinds #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DerivingStrategies #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module AgentSpec (agentSpec) where

import Control.Lens (over)
import Control.Monad.Freer (Eff)
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
  ( AgentSpec (..), AsyncEffects, Call, HaskellCell, KnownToolEffects
  , JsonSchema, Subset, Sync, SyncEffects, (:-), defaultSpec, haskellTool
  , presentWith, syncTool )
import qualified Tidepool.Agent.Contract as A
import qualified Tidepool.Agent.Context as C

newtype CurateArgs = CurateArgs { proceed :: Bool }
  deriving stock Generic
  deriving anyclass (FromJSON, JsonSchema)

data ContextTools effects mode = ContextTools
  { haskell :: mode :- HaskellCell effects
  , haskellSync :: mode :- Sync (HaskellCell (SyncEffects effects))
  , curate :: mode :- Sync (Call CurateArgs Text)
  }
  deriving Generic

agentSpec
  :: forall effects.
     ( KnownToolEffects effects, AsyncEffects effects
     , Subset effects (SyncEffects effects)
     )
  => AgentSpec (ContextTools effects) effects
agentSpec = defaultSpec
  { specTools = ContextTools
      { haskell = A.haskell (A.haskellTools @effects)
      , haskellSync = haskellTool "Edit this actor's context before the next inference."
      , curate = presentWith id $ syncTool "Curate this actor's context and select its next model." curateContext
      }
  }

curateContext :: CurateArgs -> Eff (SyncEffects effects) Text
curateContext _ = do
  _ <- C.modifyContext (over C.editableTexts (T.replace "parent-original" "compiled-handler-curated"))
  C.setNextModel "executor"
  C.setNextEffort C.High
  pure "compiled-handler-committed"
