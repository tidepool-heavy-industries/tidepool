{-# LANGUAGE OverloadedStrings #-}

module TextContract where

import Data.Text (Text)
import Data.Text qualified as T

splitOnParts :: [Text]
splitOnParts = T.splitOn "," "alpha,beta,,gamma"

intercalated :: Text
intercalated = T.intercalate " | " ["one", "two", "three"]

replaced :: Text
replaced = T.replace "cat" "dog" "the cat sat on the cat mat"

upperWordsRoundTrip :: Text
upperWordsRoundTrip = T.unwords (map T.toUpper (T.words "quick brown fox"))

nonAsciiLength :: Int
nonAsciiLength = T.length "naïve — 縮小 ✓"

packedShow :: Text
packedShow = T.pack (show (12345 :: Int, True))

charFold :: Int
charFold = T.foldl' (\count character -> count + fromEnum character) 0 "abc"

textKeyedLookup :: Maybe Int
textKeyedLookup = lookup "beta" table
  where
    table = [("alpha", 1), ("beta", 2), ("gamma", 3)] :: [(Text, Int)]
