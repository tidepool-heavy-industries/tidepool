{-# LANGUAGE GADTs #-}
{-# LANGUAGE RoleAnnotations #-}

-- | Static model families and exact choices from an admitted catalog.
-- Constructing a value expresses a selection; the runtime checks its catalog,
-- provider family, exact model and effort against the actor's immutable grants.
module Tidepool.Model.Selection
  ( Model
  , OpenAI, Argon, DeepSeek, KnownFamily
  , openAI, argon, deepSeek
  , modelSelection
  , ModelPermissions
  , ModelPermission
  , noModelPermissions
  , modelPermissions
  , allowModelCall
  , allowForkAgent
  , allowSpawnAgent
  , modelPermissionRequests
  , ArgonOptions (..)
  , DeepSeekOptions (..)
  , OpenAIOptions (..)
  , ArgonEffort (..)
  , DeepSeekReasoning (..)
  , OpenAIReasoning (..)
  ) where

import Prelude
import Data.Text (Text)
import Tidepool.Effects.Core
  ( OpenAI, Argon, DeepSeek, KnownFamily
  , ArgonEffort (..), DeepSeekReasoning (..), OpenAIReasoning (..)
  , Reasoning (..)
  )
import Tidepool.Internal.ModelControl
  ( ModelSelectionEnvelope (..), ModelOperation, ModelGrantRequest (..) )
import qualified Tidepool.Internal.ModelControl as Control

-- The nominal index prevents Data.Coerce from changing provider families.
type role Model nominal
data Model family where
  OpenAIModel :: Text -> Text -> OpenAIOptions -> Model OpenAI
  ArgonModel :: Text -> Text -> ArgonOptions -> Model Argon
  DeepSeekModel :: Text -> Text -> DeepSeekOptions -> Model DeepSeek

instance Eq (Model family) where
  left == right = modelSelection left == modelSelection right

instance Show (Model family) where
  showsPrec precedence = showsPrec precedence . modelSelection

-- | Bind a catalog identity and exact entry key to typed provider options.
openAI :: Text -> Text -> OpenAIOptions -> Model OpenAI
openAI = OpenAIModel

argon :: Text -> Text -> ArgonOptions -> Model Argon
argon = ArgonModel

deepSeek :: Text -> Text -> DeepSeekOptions -> Model DeepSeek
deepSeek = DeepSeekModel

modelSelection :: Model family -> ModelSelectionEnvelope
modelSelection (OpenAIModel catalog key options) =
  ModelSelectionEnvelope catalog key (OpenAIReasoning (openAIEffort options))
modelSelection (ArgonModel catalog key options) =
  ModelSelectionEnvelope catalog key (ArgonReasoning (argonEffort options))
modelSelection (DeepSeekModel catalog key options) =
  ModelSelectionEnvelope catalog key (DeepSeekReasoning (deepSeekEffort options))

-- | Child authority is explicit. An omitted permission value grants no model
-- operations. Repeated operation/model entries combine their selected efforts.
newtype ModelPermissions = ModelPermissions [ModelGrantRequest]
  deriving (Eq, Show)

newtype ModelPermission = ModelPermission ModelGrantRequest
  deriving (Eq, Show)

noModelPermissions :: ModelPermissions
noModelPermissions = ModelPermissions []

modelPermissions :: [ModelPermission] -> ModelPermissions
modelPermissions requests = ModelPermissions [request | ModelPermission request <- requests]

allowModelCall :: Model family -> ModelPermission
allowModelCall = permission Control.ModelCall

allowForkAgent :: Model family -> ModelPermission
allowForkAgent = permission Control.ForkAgent

allowSpawnAgent :: Model family -> ModelPermission
allowSpawnAgent = permission Control.SpawnAgent

permission :: ModelOperation -> Model family -> ModelPermission
permission operation model = case modelSelection model of
  ModelSelectionEnvelope catalog key reasoning ->
    ModelPermission (ModelGrantRequest operation catalog key [reasoning])

-- | The launch interpreter admits these requests before allocating a child.
modelPermissionRequests :: ModelPermissions -> [ModelGrantRequest]
modelPermissionRequests (ModelPermissions requests) = requests

data ArgonOptions = ArgonOptions
  { argonEffort :: ArgonEffort
  } deriving (Eq, Show)

data DeepSeekOptions = DeepSeekOptions
  { deepSeekEffort :: DeepSeekReasoning
  } deriving (Eq, Show)

data OpenAIOptions = OpenAIOptions
  { openAIEffort :: OpenAIReasoning
  } deriving (Eq, Show)
