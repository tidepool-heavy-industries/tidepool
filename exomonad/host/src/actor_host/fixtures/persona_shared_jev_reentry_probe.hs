{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Persona.ReentryProbe (ProbeResponse, arrival) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import qualified Jev.Operators as J
import Tidepool.Aeson (FromJSON, eitherDecode)
import qualified Tidepool.Effects.Authored as Files
import Tidepool.Effects.Core (FsRead)

data Marker = Marker { observation :: Text } deriving (Generic)
instance FromJSON Marker

type Choices = "remember" J.::> Maybe Marker
type Packet = J.Packet ("reentry" J.::= J.Choice Choices)
type ProbeResponse = J.Response (Packet J.Answers)

arrival :: Member FsRead effects => Eff effects ()
arrival = do
  file <- Files.readFile ".exomonad/state/notes/reentry-probe.json"
  case file of
    Left issue -> error ("reentry probe read failed: " <> show issue)
    Right body -> case eitherDecode body :: Either Text Marker of
      Left issue -> error ("reentry probe decode failed: " <> show issue)
      Right marker
        | observation marker == "ready" -> pure ()
        | otherwise -> error "reentry probe decoded unexpected marker"
