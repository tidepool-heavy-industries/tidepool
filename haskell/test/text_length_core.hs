{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T

textLength :: Text -> Int
textLength = T.length

textTake :: Int -> Text -> Text
textTake = T.take

textDrop :: Int -> Text -> Text
textDrop = T.drop
