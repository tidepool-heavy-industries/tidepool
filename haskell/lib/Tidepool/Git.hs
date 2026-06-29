{-# LANGUAGE OverloadedStrings #-}

-- | Git effect module: typed wrappers over 'Shell.runArgv' that parse
-- porcelain and log output into structured 'Value' records.
--
-- Lens-free: all 'Value' deconstruction uses 'KM.lookup' + case, not optics.
-- Parse with 'T.*' text ops; no external parsers needed.
module Tidepool.Git
  ( gitStatus
  , gitLog
  , gitBlameLine
  , gitDiff
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import Tidepool.Aeson.Value (Value, object, (.=))
import Tidepool.Effects (M)
import qualified Tidepool.Shell as Shell

-- | Parse @git status --porcelain@ into @[{\"xy\", \"file\"}]@ records.
--
-- @XY@ is the two-char status code (index + worktree). Rename lines
-- (@R old -> new@) surface the destination path only.
gitStatus :: M [Value]
gitStatus = do
  ls <- Shell.shLines ["git", "status", "--porcelain"]
  pure (map parseSLine ls)
  where
    parseSLine l =
      let (xy, rest) = T.splitAt 2 l
          file       = T.strip rest
          dest       = case T.splitOn " -> " file of
            (_:d:_) -> d
            _       -> file
      in object ["xy" .= xy, "file" .= dest]

-- | Parse the last @n@ commits into @[{\"hash\", \"author\", \"date\", \"subject\"}]@.
--
-- Uses ASCII unit-separator @\\x1f@ as the field delimiter — safe in git output.
gitLog :: Int -> M [Value]
gitLog n = do
  let fmt  = "%H\x1f%an\x1f%ai\x1f%s"
      argv = ["git", "log", "-n", T.pack (show n), "--format=" <> fmt]
  ls <- Shell.shLines argv
  pure (map parseLogLine ls)
  where
    parseLogLine l =
      case T.splitOn "\x1f" l of
        (h:a:d:s:_) -> object ["hash" .= h, "author" .= a, "date" .= d, "subject" .= s]
        _            -> object ["raw" .= l]

-- | Return the author name for a single line via @git blame --porcelain@.
--
-- Porcelain blame emits @author <name>@ as the second line of each block.
gitBlameLine :: Text -> Int -> M Text
gitBlameLine filepath lineNo = do
  let lineStr = T.pack (show lineNo)
      argv    = ["git", "blame", "--porcelain",
                 "-L", lineStr <> "," <> lineStr,
                 filepath]
  out <- Shell.sh1 argv
  pure (extractAuthor (T.lines out))
  where
    extractAuthor ls =
      case filter (T.isPrefixOf "author ") ls of
        (l:_) -> T.strip (T.drop 7 l)
        []    -> ""

-- | Return @git diff HEAD@ (staged + unstaged) as raw unified-diff text.
gitDiff :: M Text
gitDiff = Shell.sh1 ["git", "diff", "HEAD"]
