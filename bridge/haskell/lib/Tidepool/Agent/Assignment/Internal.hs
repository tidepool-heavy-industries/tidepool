{-# LANGUAGE OverloadedStrings #-}

module Tidepool.Agent.Assignment.Internal
  ( Label (..)
  , NameError (..)
  , labelFromText
  , labelText
  ) where

import Data.Char (isAsciiLower, isDigit)
import Data.Text (Text)
import qualified Data.Text as Text

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

labelText :: Label -> Text
labelText (Label value) = value
