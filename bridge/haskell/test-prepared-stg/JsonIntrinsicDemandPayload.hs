{-# LANGUAGE OverloadedStrings #-}
module JsonIntrinsicDemandPayload (importedBody, importedText) where

import qualified Data.Map.Strict as Map
import Data.Text (Text)
import Tidepool.Aeson.Value

importedBody :: Value
importedBody = Object (Map.fromList [("preserved", Bool True)])

importedText :: Text
importedText = "{\"decoded\":true}"
