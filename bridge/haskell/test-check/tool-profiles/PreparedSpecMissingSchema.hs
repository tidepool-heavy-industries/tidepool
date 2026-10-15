{-# LANGUAGE DataKinds, DeriveAnyClass, DeriveGeneric, OverloadedStrings, TypeOperators #-}
module PreparedSpecMissingSchema where
import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON)
import Tidepool.Agent.Contract

data Input = Input { question :: Text } deriving (Generic, FromJSON)
data Tools mode = Tools { ask :: mode :- Sync (Call Input Text) } deriving Generic

invalid :: Either ToolCompileError (PreparedAgentSpec Tools '[])
invalid = prepareAgentSpec (defaultSpec
  { specTools = Tools (presentWith id (syncTool "Concrete schema requirement" (\(Input text) -> pure text))) })
