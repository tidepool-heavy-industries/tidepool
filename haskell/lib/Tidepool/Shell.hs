{-# LANGUAGE OverloadedStrings #-}

-- | Shell-effect affordance: typed combinators over 'runArgv'.
--
-- A "shell-effect" module is a collection of typed 'runArgv' wrappers that
-- turn raw argv lists into domain-typed results. This module provides the
-- building blocks; 'Tidepool.Git' and 'Tidepool.Cargo' are exemplars built on
-- top of it.
--
-- Pattern: @sh1 [\"git\", \"status\", \"--porcelain\"] >>= parseLines@
-- No shell metachar expansion — @$VAR@, globs, pipes are literal.
-- Lens-free: deconstruct 'Value' with 'KM.lookup' + case, not optics.
module Tidepool.Shell
  ( sh1
  , shLines
  , shJson
  , shTry
  , splitCols
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import Tidepool.Aeson.Value (Value)
import Tidepool.Effects (M, runArgv, parseJson)

-- | Run a command (argv, no shell), strip stdout, throw on nonzero exit.
sh1 :: [Text] -> M Text
sh1 argv = do
  (ec, out, err) <- runArgv argv
  if ec == 0
    then pure (T.strip out)
    else Prelude.error ("sh: exit " ++ show ec ++ ": " ++ T.unpack (T.strip err))

-- | Run and split stdout into non-empty lines.
shLines :: [Text] -> M [Text]
shLines argv = do
  out <- sh1 argv
  let ls = T.lines out
  pure (filter (not . T.null) ls)

-- | Run and parse stdout as JSON via 'parseJson'. Throws on parse error.
shJson :: [Text] -> M Value
shJson argv = sh1 argv >>= parseJson

-- | Run; return @Right stdout@ on zero exit, @Left stderr@ on nonzero.
shTry :: [Text] -> M (Either Text Text)
shTry argv = do
  (ec, out, err) <- runArgv argv
  if ec == 0
    then pure (Right (T.strip out))
    else pure (Left (T.strip err))

-- | Split a text line on ASCII whitespace, discarding empty segments.
-- Useful for parsing fixed-column porcelain output (e.g. @git status --porcelain@).
splitCols :: Text -> [Text]
splitCols t = map T.pack (words (T.unpack t))
