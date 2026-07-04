{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DuplicateRecordFields, OverloadedRecordDot, DeriveGeneric #-}
-- Commit/StatusEntry/FileDelta/Proc/Hit/FileMeta are defined in
-- Tidepool.Records.Bridged (generated from the Rust wire-structs); their
-- ToJSON instances live here, so they are orphan instances by construction.
{-# OPTIONS_GHC -Wno-orphans #-}

-- | Shared record vocabulary for the eval surface.
--
-- Field access uses record-dot (@x.field@): with 'DuplicateRecordFields' the
-- bare selectors (@path@, @line@, @text@) are shared across record types,
-- so dot-syntax is required to disambiguate.
--
-- NB: this module must NOT import 'Tidepool.Prelude' (that would create an
-- import cycle — Prelude re-exports this module).
module Tidepool.Records
  ( Proc(..), ok, Hit(..)
  , FileMeta(..)
  , UpdateOutcome(..)
  , UpdateOneOutcome(..)
  , UpdateAllOutcome(..)
  , InsertAfterOutcome(..)
  , WriteOutcome(..)
  , Commit(..)
  , StatusEntry(..)
  , FileDelta(..)
  ) where

import Prelude (Int, Bool(..), Eq, Show, Maybe(..), (==))
import Data.Text (Text)
import Tidepool.Aeson.Value (ToJSON(..), object, (.=))
-- Wire records: GENERATED from the Rust structs (single source of truth).
-- Re-exported below so `Tidepool.Prelude` (→ user evals) sees them unchanged.
import Tidepool.Records.Bridged (Commit(..), StatusEntry(..), FileDelta(..), Proc(..), Hit(..), FileMeta(..))

-- | Did the process exit successfully (exit code 0)?
ok :: Proc -> Bool
ok p = p.exitCode == 0

instance ToJSON Proc where
  toJSON p = object ["exitCode" .= p.exitCode, "stdout" .= p.stdout, "stderr" .= p.stderr, "ok" .= ok p]

instance ToJSON Hit where
  toJSON h = object ["path" .= h.path, "line" .= h.line, "text" .= h.text]

-- | 'ToJSON' for the filesystem-metadata record. The JSON keys stay
-- snake_case for back-compat with @^? key "size" . _Int@ style consumers.
instance ToJSON FileMeta where
  toJSON m = object ["size" .= m.size, "is_file" .= m.isFile, "is_dir" .= m.isDir]

-- | Outcome of @planUpdate@ (the dry-run str-replace). Documents the four
-- shapes the verb used to hand back as an opaque Value:
--
--   * 'UpdateRejected' — the replace cannot proceed (file missing, empty
--     @old@, not found, or ambiguous). The 'Maybe' 'Int' carries the match
--     count for the ambiguous case only.
--   * 'UpdateNoChange' — it would apply, but produces identical content.
--   * 'UpdateDiff' — the rendered review diff.
data UpdateOutcome
  = UpdateRejected { reason :: Text, ambiguousCount :: Maybe Int }
  | UpdateNoChange
  | UpdateDiff { diff :: Text }
  deriving (Show, Eq)

instance ToJSON UpdateOutcome where
  toJSON (UpdateRejected r Nothing)  = object ["ok" .= False, "reason" .= r]
  toJSON (UpdateRejected r (Just c)) = object ["ok" .= False, "reason" .= r, "count" .= c]
  toJSON UpdateNoChange              = object ["ok" .= True, "changed" .= False]
  toJSON (UpdateDiff d)              = object ["ok" .= True, "changed" .= True, "diff" .= d]

-- | Outcome of @update@ (exact str-replace, exactly-once). Reported as DATA,
-- never thrown (mirrors 'InsertAfterOutcome'):
--
--   * 'UpdateOneRejected' — @old@ was empty, the file is missing, @old@ was
--     not found, or @old@ matched 2+ places. The 'Maybe' 'Int' carries the
--     match count for the ambiguous case only.
--   * 'UpdateOneApplied' — the replace was applied.
data UpdateOneOutcome
  = UpdateOneRejected { reason :: Text, ambiguousCount :: Maybe Int }
  | UpdateOneApplied
  deriving (Show, Eq)

instance ToJSON UpdateOneOutcome where
  toJSON (UpdateOneRejected r Nothing)  = object ["ok" .= False, "reason" .= r]
  toJSON (UpdateOneRejected r (Just c)) = object ["ok" .= False, "reason" .= r, "count" .= c]
  toJSON UpdateOneApplied                = object ["ok" .= True]

-- | Outcome of @updateAll@ (replace-EVERY-occurrence str-replace). Reported as
-- DATA, never thrown (mirrors 'UpdateOutcome'):
--
--   * 'UpdateAllRejected' — @old@ was empty, or matched nowhere, or the file
--     is missing; nothing written.
--   * 'UpdateAllApplied' — every occurrence replaced; carries the count.
data UpdateAllOutcome
  = UpdateAllRejected { reason :: Text }
  | UpdateAllApplied { count :: Int }
  deriving (Show, Eq)

instance ToJSON UpdateAllOutcome where
  toJSON (UpdateAllRejected r) = object ["ok" .= False, "reason" .= r]
  toJSON (UpdateAllApplied c)  = object ["ok" .= True, "count" .= c]

-- | Outcome of @insertAfter@ (insert a block after the unique anchor line).
-- Reported as DATA, never thrown (mirrors 'UpdateOutcome'):
--
--   * 'InsertAfterRejected' — the file is missing, or the anchor matched zero
--     or 2+ lines. The 'Maybe' 'Int' carries the anchor's match count (only
--     present when the rejection is anchor-related, not a missing file).
--   * 'InsertAfterApplied' — the block was inserted.
data InsertAfterOutcome
  = InsertAfterRejected { reason :: Text, matchCount :: Maybe Int }
  | InsertAfterApplied
  deriving (Show, Eq)

instance ToJSON InsertAfterOutcome where
  toJSON (InsertAfterRejected r Nothing)  = object ["ok" .= False, "reason" .= r]
  toJSON (InsertAfterRejected r (Just n)) = object ["ok" .= False, "reason" .= r, "matches" .= n]
  toJSON InsertAfterApplied                = object ["ok" .= True]

-- | Outcome of @writeChecked@ (compute-check-commit) and @writeCheckedIf@
-- (content-hash compare-and-swap). 'Written' carries the file and the number
-- of checks/preconditions that held; 'WriteBlocked' carries the file and the
-- names of the failed checks (nothing was written); 'WriteConflict' is the CAS
-- precondition miss — the file's current hash did not match the expected one,
-- so nothing was written. It carries the file plus @expected@ vs @actual@
-- blake3 hashes ('Nothing' = the file was absent) so a caller can re-read,
-- recompute, and retry. Conflicts-as-data, matching the Diff/Edit philosophy.
data WriteOutcome
  = Written { file :: Text, checks :: Int }
  | WriteBlocked { file :: Text, failed :: [Text] }
  | WriteConflict { file :: Text, expected :: Maybe Text, actual :: Maybe Text }
  deriving (Show, Eq)

instance ToJSON WriteOutcome where
  toJSON (Written f c)         = object ["file" .= f, "written" .= True, "checks" .= c]
  toJSON (WriteBlocked f xs)   = object ["file" .= f, "written" .= False, "failed" .= xs]
  toJSON (WriteConflict f e a) = object ["file" .= f, "written" .= False, "conflict" .= True, "expected" .= e, "actual" .= a]

-- | A git commit returned by 'gitLog' or 'gitShow'.
-- The @data Commit@ decl is GENERATED (Tidepool.Records.Bridged); only its
-- 'ToJSON' instance lives here (orphan by construction).
instance ToJSON Commit where
  toJSON c = object
    [ "sha"     .= c.sha
    , "subject" .= c.subject
    , "author"  .= c.author
    , "date"    .= c.date
    , "files"   .= c.files
    ]

-- | A single entry from 'gitStatus' (porcelain v1).
-- 'state' is the two-character XY code (e.g. \"M \", \"??\", \"A \").
-- The @data StatusEntry@ decl is GENERATED (Tidepool.Records.Bridged).
instance ToJSON StatusEntry where
  toJSON e = object ["path" .= e.path, "state" .= e.state]

-- | Per-file diff statistics from 'gitDiffStat'.
-- 'binary' is True when git reports @-/@- instead of line counts.
-- The @data FileDelta@ decl is GENERATED (Tidepool.Records.Bridged).
instance ToJSON FileDelta where
  toJSON d = object
    [ "path"   .= d.path
    , "adds"   .= d.adds
    , "dels"   .= d.dels
    , "binary" .= d.binary
    ]
