{-# LANGUAGE OverloadedStrings, OverloadedRecordDot #-}

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
  ( sh
  , sh1
  , shLines
  , shJson
  , shTry
  , splitCols
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import Tidepool.Aeson.Value (Value)
import Tidepool.Aeson.FromJSON (eitherDecode)
import Tidepool.Records (Proc(..), ok)
import Tidepool.Effects (M, run, runArgv, liftEither)

-- | Run a shell-string command, strip stdout, throw on nonzero exit — the
-- shell-string sibling of 'sh1'. Shell metachars (@$VAR@, globs, pipes, @&&@)
-- are LIVE here, unlike 'sh1's argv (no expansion at all). A nonzero exit is
-- DATA, not a spawn failure: when it's expected, inspect it directly via
-- `run` (`Right p <- run cmd; p.exitCode`) instead of `sh`.
sh :: Text -> M Text
sh cmd = do
  p <- run cmd >>= liftEither
  if ok p
    then pure (T.strip p.stdout)
    else Prelude.error ("sh: exit " ++ show p.exitCode ++ ": " ++ T.unpack (T.strip p.stderr))

-- | Run a command (argv, no shell), strip stdout, throw on nonzero exit.
-- `runArgv` is typed (#335): a spawn failure aborts via `liftEither`, same as
-- pre-#335 (a nonzero exit is not a spawn failure — it's still checked below).
sh1 :: [Text] -> M Text
sh1 argv = do
  p <- runArgv argv >>= liftEither
  if ok p
    then pure (T.strip p.stdout)
    else Prelude.error ("sh: exit " ++ show p.exitCode ++ ": " ++ T.unpack (T.strip p.stderr))

-- | Run and split stdout into non-empty lines.
shLines :: [Text] -> M [Text]
shLines argv = do
  out <- sh1 argv
  let ls = T.lines out
  pure (filter (not . T.null) ls)

-- | Run and parse stdout as JSON via the pure 'eitherDecode'. Throws on parse
-- error (the `Left msg` aborts via `liftEither`).
shJson :: [Text] -> M Value
shJson argv = sh1 argv >>= liftEither . eitherDecode

-- | Run; return @Right stdout@ on zero exit, @Left stderr@ on nonzero. A
-- spawn failure still aborts (via `liftEither`) — this @Either@ is purely the
-- exit-code check, distinct from `runArgv`'s own typed `ExecError`.
shTry :: [Text] -> M (Either Text Text)
shTry argv = do
  p <- runArgv argv >>= liftEither
  if ok p
    then pure (Right (T.strip p.stdout))
    else pure (Left (T.strip p.stderr))

-- | Split a text line on ASCII whitespace, discarding empty segments.
-- Useful for parsing fixed-column porcelain output (e.g. @git status --porcelain@).
splitCols :: Text -> [Text]
splitCols t = map T.pack (words (T.unpack t))
