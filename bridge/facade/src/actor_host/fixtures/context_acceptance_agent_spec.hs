{-# LANGUAGE ConstraintKinds #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
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
  , JsonSchema, Sync, SyncEffects, (:-), defaultSpec, syncTool )
import qualified Tidepool.Agent.Contract as A
import qualified Tidepool.Agent.Context as C

newtype CurateArgs = CurateArgs { proceed :: Bool }
  deriving (Generic, FromJSON, JsonSchema)

data ContextTools effects mode = ContextTools
  { haskell :: mode :- HaskellCell effects
  , curate :: mode :- Sync (Call CurateArgs Text)
  }
  deriving Generic

agentSpec
  :: forall effects. (KnownToolEffects effects, AsyncEffects effects)
  => AgentSpec (ContextTools effects) effects
agentSpec = defaultSpec
  { specTools = ContextTools
      { haskell = A.haskell (A.haskellTools @effects)
      , curate = syncTool "Curate this actor's context and select its next model." curateContext
      }
  }

curateContext :: CurateArgs -> Eff (SyncEffects effects) Text
curateContext _ = do
  _ <- C.modifyContext (over C.editableTexts (T.replace "parent-original" "compiled-handler-curated"))
  C.setNextModel "executor"
  pure "compiled-handler-committed"
