{-# LANGUAGE OverloadedStrings #-}
module ConstructorAdmissionAliases where

import qualified Data.Text as Text

text :: Text.Text
text = "hello"

maybe :: Maybe Int
maybe = Just 42

either :: Either Int Int
either = Left 43
