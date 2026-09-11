{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Project.Tools where

import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import Tidepool.Aeson.FromJSON (FromJSON)

data EchoInput = EchoInput { text :: Text, copies :: Int }
  deriving (Generic, FromJSON, JsonSchema)

data TestTools mode = TestTools
  { rawEcho :: mode :- RawCall Text
  , repeatText :: mode :- Call EchoInput Text
  } deriving Generic

tools :: Applicative m => TestTools (AsServerT m)
tools = TestTools
  { rawEcho = rawTool "Echo literal input." $ \input ->
      if input == "fail" then error "expected raw handler failure"
      else pure ("frozen:" <> input)
  , repeatText = tool "Repeat supplied text." $ \EchoInput {text = value, copies = count} ->
      pure (T.replicate count value)
  }
