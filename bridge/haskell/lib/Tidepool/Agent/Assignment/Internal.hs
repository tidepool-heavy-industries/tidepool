{-# LANGUAGE OverloadedStrings #-}

module Tidepool.Agent.Assignment.Internal
  ( Label (..)
  , NameError (..)
  , labelFromText
  , labelText
  , IsWatchLabel (..)
  ) where

import Data.Char (isAsciiLower, isDigit)
import Data.Text (Text)
import qualified Data.Text as Text

data Label = Label Text
  deriving (Show, Eq, Ord)

-- | A value buildable from already-validated, kebab-case label text: the
-- anchor that lets @[label|...|]@ resolve to whichever concrete label type
-- its use site expects (a plain 'Label' here, or, through
-- 'Tidepool.Agent.Watch.Internal', a 'Tidepool.Agent.Watch.Internal.WatchLabel')
-- instead of the quasiquote committing to one type and every other site
-- needing its own conversion.
class IsWatchLabel a where
  fromValidatedLabelText :: Text -> a

instance IsWatchLabel Label where
  fromValidatedLabelText = Label

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
