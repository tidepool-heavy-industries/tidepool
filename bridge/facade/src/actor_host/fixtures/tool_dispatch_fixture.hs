{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

module ToolDispatchFixture (dispatchChecks) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Aeson.Value (Value (..), object, toJSON, (.=))
import Tidepool.Agent.Contract

newtype DispatchInput = DispatchInput {content :: Text}
  deriving (Generic, FromJSON, JsonSchema)

data DispatchTools mode = DispatchTools
  { structuredProbe :: mode :- Call DispatchInput Text
  , rawProbe :: mode :- RawCall Text
  }
  deriving (Generic)

record :: DispatchTools (AsServerT (Eff effects))
record = DispatchTools
  { structuredProbe = presentWith id $ tool "Return the decoded text." (\(DispatchInput value) -> pure value)
  , rawProbe = presentWith id $ rawTool "Return literal text." pure
  }

dispatchChecks :: Eff effects Bool
dispatchChecks = case compileTools record of
  Left _ -> pure False
  Right compiled -> do
    unknown <- dispatch compiled "absent" Null
    invalidStructured <- dispatch compiled "structured_probe" (toJSON (17 :: Int))
    invalidRaw <- dispatch compiled "raw_probe" (object [])
    structured <- dispatch compiled "structured_probe" (object ["content" .= ("decoded" :: Text)])
    raw <- dispatch compiled "raw_probe" (toJSON ("literal" :: Text))
    pure
      ( unknown == Left (UnknownTool "absent")
      && invalid "structured_probe" invalidStructured
      && invalid "raw_probe" invalidRaw
      && structured == Right (ToolDispatchSuccess (toJSON ("decoded" :: Text)) "decoded")
      && raw == Right (ToolDispatchSuccess (toJSON ("literal" :: Text)) "literal")
      )
  where
    invalid expected result = case result of
      Left (InvalidToolInput name _) -> name == expected
      _ -> False
