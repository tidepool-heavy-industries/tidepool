{-# LANGUAGE OverloadedStrings #-}

-- Paths and line counts from the candidate diff inspected by Project.Work.
module Project.Evidence (numstatFiles) where

import Data.Char (isDigit)
import Data.Text (Text)
import qualified Data.Text as Text

-- `git diff --numstat` lines: added, deleted, path.
numstatFiles :: Text -> [(Int, Int, Text)]
numstatFiles stat =
  [ (number added, number deleted, path)
  | line <- Text.lines stat
  , (added : deleted : path : _) <- [Text.splitOn "\t" line]
  ]
  where
    -- Binary files print "-" for both counts and contribute zero lines.
    number = Text.foldl' digit 0 . Text.strip
    digit total character
      | isDigit character = total * 10 + (fromEnum character - fromEnum '0')
      | otherwise = total
