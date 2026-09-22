{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- Facts about a candidate revision, and the mechanical checks run over them.
-- No actors, no Jev: everything here is pure, or reads git output already in
-- hand. Read by Project.Merge (which stores its own history as the same
-- `HistoryEntry` shape) and by Project.Review (which derives `Evidence` for
-- each candidate and turns a failed `CheckResult` into a repair request).
module Project.Evidence
  ( -- Evidence and the history
    CheckSource (..)
  , CheckResult (..)
  , Evidence (..)
  , HistoryEntry (..)
  , NoticeKind (..)
  , Notice (..)
  , renderNotice
  , shortOid
    -- The checks, in code
  , numstatFiles
  , binaryFiles
  , coverageCheck
  , ownershipCheck
  , missingTests
  , okLines
  , fitHunks
  ) where

import Data.Char (isDigit)
import Data.Text (Text)
import qualified Data.Text as Text

import Tidepool.Actors.Shoal (GitOid)
import Tidepool.Worktree (renderGitOid)

import Project.Reflex (Reflex)

-- ---------------------------------------------------------------------------
-- Evidence, checks, history
-- ---------------------------------------------------------------------------

-- Who established a fact. A check the child claimed in its own output is a
-- different kind of fact from one this review ran, and a notice says which.
data CheckSource = ChildReported | RanHere
  deriving (Eq)

instance Show CheckSource where
  show ChildReported = "child-reported"
  show RanHere = "ran-here"

data CheckResult = CheckResult
  { checkName :: Text
  , checkSource :: CheckSource
  , checkPassed :: Bool
  , checkDetail :: Text
  } deriving (Eq)

instance Show CheckResult where
  show result = Text.unpack (checkName result)
    ++ (if checkPassed result then "=ok" else "=FAIL")
    ++ "(" ++ show (checkSource result) ++ ")"

-- Derived in the review's own checkout from the OID the reply carried, never
-- from the child's file list.
data Evidence = Evidence
  { evidenceCandidate :: GitOid
  , evidenceStat :: Text
  , evidenceHunks :: Text
  , evidenceOutput :: Text
  , evidenceChecks :: [CheckResult]
  , evidenceReflex :: Maybe Reflex
  }

instance Show Evidence where
  show evidence = Text.unpack (shortOid (evidenceCandidate evidence))
    ++ " " ++ show (evidenceChecks evidence)
    ++ " reflex=" ++ maybe "unmatched" show (evidenceReflex evidence)

data HistoryEntry = HistoryEntry
  { entryIndex :: Int
  , entrySeam :: Text
  , entryCandidate :: Maybe GitOid
  , entrySource :: CheckSource
  , entryKey :: Text
  , entryDetail :: Text
  , entryAction :: Text
  }

-- One line per decision: the root reads a whole review's history in a
-- screenful.
instance Show HistoryEntry where
  show entry = "[" ++ show (entryIndex entry) ++ "] "
    ++ Text.unpack (entrySeam entry)
    ++ " " ++ Text.unpack (maybe "-" shortOid (entryCandidate entry))
    ++ " " ++ show (entrySource entry)
    ++ " " ++ Text.unpack (entryKey entry)
    ++ ": " ++ Text.unpack (entryDetail entry)
    ++ " -> " ++ Text.unpack (entryAction entry)

data NoticeKind = Alert | Info
  deriving (Show, Eq)

-- Actionable in one read: the task, the candidate, what already passed, the
-- condition with its source, the history row to open, and the suggested
-- move.
data Notice = Notice
  { noticeKind :: NoticeKind
  , noticeTask :: Text
  , noticeCandidate :: Maybe GitOid
  , noticeChecksPassed :: [Text]
  , noticeCondition :: Text
  , noticeSource :: CheckSource
  , noticeEntry :: Int
  , noticeSuggested :: Text
  } deriving (Show, Eq)

renderNotice :: Notice -> Text
renderNotice notice = case noticeKind notice of
  Info -> "merged " <> noticeTask notice <> " " <> candidate
    <> " checks=[" <> Text.intercalate "," (noticeChecksPassed notice) <> "]"
  Alert -> noticeTask notice <> " " <> candidate
    <> " needs: " <> noticeCondition notice
    <> " (" <> Text.pack (show (noticeSource notice)) <> ")"
    <> " [history " <> Text.pack (show (noticeEntry notice)) <> "]"
    <> " -> " <> noticeSuggested notice
  where candidate = maybe "-" shortOid (noticeCandidate notice)

shortOid :: GitOid -> Text
shortOid = Text.take 7 . renderGitOid

-- ---------------------------------------------------------------------------
-- The checks, in code. None of these is a judgment and none of them asks Jev.
-- ---------------------------------------------------------------------------

-- `git diff --numstat` lines: added, deleted, path.
numstatFiles :: Text -> [(Int, Int, Text)]
numstatFiles stat =
  [ (number added, number deleted, path)
  | line <- Text.lines stat
  , (added : deleted : path : _) <- [Text.splitOn "\t" line]
  ]
  where
    -- A binary file prints "-" for both counts; that reads as 0, which is
    -- what the coverage comparison wants.
    number = Text.foldl' digit 0 . Text.strip
    digit total character
      | isDigit character = total * 10 + (fromEnum character - fromEnum '0')
      | otherwise = total

-- Every file in the stat has a hunk, and the stat's line counts equal the
-- hunks'. A review whose evidence is incomplete does not get asked.
-- `git diff --numstat` prints "-" for both counts of a binary change; such a
-- file has no reviewable hunk, so it fails coverage explicitly instead of
-- reading as a zero-line text change.
binaryFiles :: Text -> [Text]
binaryFiles stat =
  [ path | line <- Text.lines stat, ("-" : "-" : path : _) <- [Text.splitOn "\t" line] ]

coverageCheck :: Text -> Text -> CheckResult
coverageCheck stat hunks =
  CheckResult "coverage" RanHere (null binary && null missing && countsAgree) detail
  where
    binary = binaryFiles stat
    files = numstatFiles stat
    missing = [path | (_, _, path) <- files, not (path `Text.isInfixOf` hunks)]
    statAdded = sum [added | (added, _, _) <- files]
    statDeleted = sum [deleted | (_, deleted, _) <- files]
    hunkAdded = length [line | line <- Text.lines hunks, isBody '+' line]
    hunkDeleted = length [line | line <- Text.lines hunks, isBody '-' line]
    isBody marker line = Text.isPrefixOf (Text.singleton marker) line
      && not (Text.isPrefixOf (Text.replicate 3 (Text.singleton marker)) line)
    countsAgree = statAdded == hunkAdded && statDeleted == hunkDeleted
    detail
      | not (null binary) = "binary change, not reviewable as hunks: " <> Text.intercalate ", " binary
      | not (null missing) = "no hunk for " <> Text.intercalate ", " missing
      | not countsAgree = "stat " <> Text.pack (show (statAdded, statDeleted))
          <> " vs hunks " <> Text.pack (show (hunkAdded, hunkDeleted))
      | otherwise = Text.pack (show (length files)) <> " files, counts agree"

ownershipCheck :: [Text] -> Text -> CheckResult
ownershipCheck owned stat = CheckResult "ownership" RanHere (null strays) detail
  where
    strays = [path | (_, _, path) <- numstatFiles stat, path `notElem` owned]
    detail
      | null strays = "only " <> Text.intercalate ", " owned
      | otherwise = "outside ownership: " <> Text.intercalate ", " strays

-- A required test is present when the hunks introduce its name and the child's
-- literal output reports it. Both halves are checked; neither is a claim.
missingTests :: [Text] -> Text -> Text -> [Text]
missingTests required hunks output =
  [ name
  | name <- required
  , not (name `Text.isInfixOf` hunks) || name `notElem` okLines output
  ]

-- The names reported passing by libtest (`test tests::foo ... ok`) and by
-- nextest (`PASS [ 0.0s] crate tests::foo`). Both the qualified path and its
-- last segment count, so a contract may name either.
okLines :: Text -> [Text]
okLines output = concat
  [ [path, lastSegment path]
  | line <- Text.lines output
  , let fields = Text.words line
  , path <- case fields of
      ("test" : name : rest) | "ok" `elem` rest -> [name]
      ("PASS" : rest) -> take 1 (reverse rest)
      _ -> []
  ]
  where
    lastSegment name = case reverse (Text.splitOn "::" name) of
      segment : _ -> segment
      [] -> name

-- Whole files that fit the budget, in order, and the files left out. A
-- packet either carries a file's hunk completely or names it as omitted.
fitHunks :: Int -> Text -> (Text, [Text])
fitHunks budget hunks
  | Text.length hunks <= budget = (hunks, [])
  | otherwise = go 0 [] [] (Text.splitOn "\ndiff --git " hunks)
  where
    go _ kept omitted [] = (Text.intercalate "\ndiff --git " (reverse kept), reverse omitted)
    go used kept omitted (section : rest)
      | used + Text.length section + 12 <= budget = go (used + Text.length section + 12) (section : kept) omitted rest
      | otherwise = go used kept (fileOf section : omitted) rest
    fileOf section = case Text.words (Text.takeWhile (/= '\n') section) of
      (_ : target : _) -> Text.drop 2 target
      _ -> Text.take 40 section
