{-# LANGUAGE OverloadedStrings, ScopedTypeVariables, OverloadedRecordDot #-}

-- | Cargo effect module: typed wrappers over 'runArgv' that parse
-- @--message-format=json@ line-delimited output into 'Value' lists.
--
-- Lens-free: deconstruct 'Value' with 'KM.lookup' + case on
-- 'Object'\/'Array'\/'String', not optics (@^?@\/@key@\/@_String@).
--
-- Example — collect only compiler errors:
-- > msgs <- cargoCheck []
-- > let errs = [ v | v@(Object m) <- msgs
-- >                , Just (String "compiler-message") <- [KM.lookup "reason" m] ]
module Tidepool.Cargo
  ( cargoCheck
  , cargoClippy
  , cargoMetadata
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import Tidepool.Aeson.Value (Value)
import Tidepool.Records (Proc(..))
import Tidepool.Effects (M, runArgv, tryParseJson)
import qualified Tidepool.Shell as Shell

-- | Run @cargo check --message-format=json [extras]@ and return each JSON
-- line as a 'Value'. @cargo check@ exits nonzero when there are errors, but
-- the JSON diagnostics are on stdout — we capture stdout regardless of exit
-- code and return the parsed lines.
--
-- Filter on the @\"reason\"@ field:
-- @\"compiler-message\"@ — error\/warning with @message.{code,rendered,spans}@;
-- @\"compiler-artifact\"@ — successful crate build.
cargoCheck :: [Text] -> M [Value]
cargoCheck extras = runCargoJson ("check" : "--message-format=json" : extras)

-- | Run @cargo clippy --message-format=json [extras]@. Same shape as 'cargoCheck'.
cargoClippy :: [Text] -> M [Value]
cargoClippy extras = runCargoJson ("clippy" : "--message-format=json" : extras)

-- | Run @cargo metadata --format-version=1@ and return the parsed 'Value'.
--
-- The top-level object has @\"packages\"@, @\"workspace_members\"@,
-- @\"workspace_root\"@, @\"resolve\"@, etc. Deconstruct with 'KM.lookup':
-- > case v of { Object m -> KM.lookup "packages" m; _ -> Nothing }
cargoMetadata :: M Value
cargoMetadata = Shell.shJson ["cargo", "metadata", "--format-version=1"]

-- ---------------------------------------------------------------------------
-- Internal helpers
-- ---------------------------------------------------------------------------

-- | Run @cargo <subArgs>@ via shell-free argv, capture stdout (regardless of
-- exit code — cargo exits nonzero on errors but JSON goes to stdout), then
-- parse each non-empty line as JSON. Lines that are not valid JSON (e.g.
-- progress messages in some terminal configurations) are silently skipped.
runCargoJson :: [Text] -> M [Value]
runCargoJson subArgs = do
  p <- runArgv ("cargo" : subArgs)
  let ls = filter (not . T.null) (T.lines p.stdout)
  vs <- mapM parseLine ls
  pure (concat vs)
  where
    parseLine :: Text -> M [Value]
    parseLine l = do
      r <- (tryParseJson l :: M (Either Text Value))
      case r of
        Right v -> pure [v]
        Left _  -> pure []
