{-# LANGUAGE DeriveFunctor #-}

-- | Data shared by a child launch and a request to an existing actor.
module Tidepool.Agent.Assignment
  ( Label
  , NameError (..)
  , labelFromText
  , labelText
  , Assignment (..)
  , SettlementReporting (..)
  , assignment
  ) where

import Data.String (IsString (fromString))
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude

import Tidepool.Agent.Assignment.Internal (Label, NameError (..), labelFromText, labelText)
import Tidepool.Duration (Duration)
import Tidepool.Effects.Core (Model (..))

instance IsString Model where
  fromString = Alias . Text.pack

data SettlementReporting = NotifyOwner | Silent
  deriving (Show, Eq)

data Assignment input = Assignment
  { assignmentLabel :: Label
  , input :: input
  , guidance :: Maybe Text
  , deadline :: Maybe Duration
  , report :: SettlementReporting
  }
  deriving (Show, Eq, Functor)

assignment :: Label -> input -> Assignment input
assignment name value = Assignment name value Nothing Nothing NotifyOwner
