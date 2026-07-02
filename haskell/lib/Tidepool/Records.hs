{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DuplicateRecordFields, OverloadedRecordDot, DeriveGeneric #-}

-- | Shared record vocabulary for the eval surface.
--
-- Three named records replacing the ad-hoc positional tuples that verbs used
-- to hand back:
--
--   * 'Proc' — a finished subprocess, replacing @(Int, Text, Text)@.
--   * 'Hit'  — a search match, replacing @(FilePath, Int, Text)@.
--   * 'Doc'  — a file's contents, replacing @(path, content)@.
--
-- Field access uses record-dot (@x.field@): with 'DuplicateRecordFields' the
-- bare selectors (@path@, @line@, @text@) are shared between 'Hit' and 'Doc',
-- so dot-syntax is required to disambiguate.
--
-- NB: this module must NOT import 'Tidepool.Prelude' (that would create an
-- import cycle — Prelude re-exports this module).
module Tidepool.Records
  ( Proc(..), ok, Hit(..), Doc(..)
  , FileMeta(..)
  , UpdateOutcome(..)
  , WriteOutcome(..)
  , Commit(..)
  , StatusEntry(..)
  , FileDelta(..)
  ) where

import Prelude (Int, Bool(..), Eq, Show, Maybe(..), (==))
import Data.Text (Text)
import Tidepool.Aeson.Value (ToJSON(..), object, (.=))

-- | A finished subprocess. Replaces @(Int, Text, Text)@ (exit code, stdout, stderr).
data Proc = Proc { exitCode :: Int, stdout :: Text, stderr :: Text } deriving (Show, Eq)

-- | Did the process exit successfully (exit code 0)?
ok :: Proc -> Bool
ok p = p.exitCode == 0

instance ToJSON Proc where
  toJSON p = object ["exitCode" .= p.exitCode, "stdout" .= p.stdout, "stderr" .= p.stderr, "ok" .= ok p]

-- | A search match. Replaces @(FilePath, Int, Text)@ (path, 1-based line, matched text)
-- everywhere (grepGlob / searchFiles / matchLocs / GotchaGuard.Hit).
data Hit = Hit { path :: Text, line :: Int, text :: Text } deriving (Show, Eq)

instance ToJSON Hit where
  toJSON h = object ["path" .= h.path, "line" .= h.line, "text" .= h.text]

-- | A file's contents. Replaces readGlob's @(path, content)@ pair.
data Doc = Doc { path :: Text, body :: Text } deriving (Show, Eq)

instance ToJSON Doc where
  toJSON d = object ["path" .= d.path, "body" .= d.body]

-- | Filesystem metadata for a path. Replaces the opaque @{size, is_file,
-- is_dir}@ Value that @fsMeta@\/@fsMetadata@ used to return; a missing path is
-- 'Nothing' at the @M (Maybe FileMeta)@ level (JSON @null@). The JSON keys stay
-- snake_case for back-compat with @^? key "size" . _Int@ style consumers.
data FileMeta = FileMeta { size :: Int, isFile :: Bool, isDir :: Bool } deriving (Show, Eq)

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

-- | Outcome of @writeChecked@ (compute-check-commit). 'Written' carries the
-- file and the number of checks that held; 'WriteBlocked' carries the file and
-- the names of the failed checks (nothing was written).
data WriteOutcome
  = Written { file :: Text, checks :: Int }
  | WriteBlocked { file :: Text, failed :: [Text] }
  deriving (Show, Eq)

instance ToJSON WriteOutcome where
  toJSON (Written f c)       = object ["file" .= f, "written" .= True, "checks" .= c]
  toJSON (WriteBlocked f xs) = object ["file" .= f, "written" .= False, "failed" .= xs]

-- | A git commit returned by 'gitLog' or 'gitShow'.
data Commit = Commit
  { sha     :: Text
  , subject :: Text
  , author  :: Text
  , date    :: Text
  , files   :: [Text]
  } deriving (Show, Eq)

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
data StatusEntry = StatusEntry
  { path  :: Text
  , state :: Text
  } deriving (Show, Eq)

instance ToJSON StatusEntry where
  toJSON e = object ["path" .= e.path, "state" .= e.state]

-- | Per-file diff statistics from 'gitDiffStat'.
-- 'binary' is True when git reports @-/@- instead of line counts.
data FileDelta = FileDelta
  { path   :: Text
  , adds   :: Int
  , dels   :: Int
  , binary :: Bool
  } deriving (Show, Eq)

instance ToJSON FileDelta where
  toJSON d = object
    [ "path"   .= d.path
    , "adds"   .= d.adds
    , "dels"   .= d.dels
    , "binary" .= d.binary
    ]
