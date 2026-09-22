{-# LANGUAGE DeriveFunctor #-}
{-# LANGUAGE OverloadedStrings #-}

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

import Data.Char (isAsciiLower, isDigit)
import Data.String (IsString (fromString))
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude

import Tidepool.Duration (Duration)
import Tidepool.Effects.Core (Model (..))

instance IsString Model where
  fromString = Alias . Text.pack

data Label = Label Text
  deriving (Show, Eq, Ord)

data NameError = EmptyName | InvalidKebabName Text | NameTooLong Text
  deriving (Show, Eq)

labelFromText :: Text -> Either NameError Label
labelFromText value
  | Text.null value = Left EmptyName
  | Text.length value > 48 = Left (NameTooLong value)
  | Text.head value == '-' || Text.last value == '-' = Left (InvalidKebabName value)
  | "--" `Text.isInfixOf` value = Left (InvalidKebabName value)
  | Text.all valid value = Right (Label value)
  | otherwise = Left (InvalidKebabName value)
  where valid character = isAsciiLower character || isDigit character || character == '-'

instance IsString Label where
  fromString = either (error . show) id . labelFromText . Text.pack

labelText :: Label -> Text
labelText (Label value) = value

data SettlementReporting = NotifyOwner | Silent
  deriving (Show, Eq)

data Assignment input = Assignment
  { label :: Label
  , input :: input
  , guidance :: Maybe Text
  , deadline :: Maybe Duration
  , report :: SettlementReporting
  }
  deriving (Show, Eq, Functor)

assignment :: Label -> input -> Assignment input
assignment name value = Assignment name value Nothing Nothing NotifyOwner
