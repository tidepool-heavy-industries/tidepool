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
module Tidepool.Records (Proc(..), ok, Hit(..), Doc(..)) where

import Prelude (Int, Bool, Eq, Show, (==))
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
