-- | Provider-specific options shared by frozen workspace model choices.
module Tidepool.Model.Selection
  ( ModelChoice (..)
  , ModelSelection (..)
  , ArgonOptions (..)
  , DeepSeekOptions (..)
  , OpenAIOptions (..)
  , ArgonEffort (..)
  , DeepSeekReasoning (..)
  , OpenAIReasoning (..)
  ) where

import Prelude
import Tidepool.Effects.Core
  ( ModelSelection (..)
  , ArgonEffort (..)
  , DeepSeekReasoning (..)
  , OpenAIReasoning (..)
  )

-- | A selection is checked against the actor's admitted catalog by the host.
class ModelChoice a where
  modelSelection :: a -> ModelSelection

data ArgonOptions = ArgonOptions
  { argonEffort :: ArgonEffort
  } deriving (Eq, Show)

data DeepSeekOptions = DeepSeekOptions
  { deepSeekEffort :: DeepSeekReasoning
  } deriving (Eq, Show)

data OpenAIOptions = OpenAIOptions
  { openAIEffort :: OpenAIReasoning
  } deriving (Eq, Show)
