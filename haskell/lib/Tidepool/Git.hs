{-# LANGUAGE OverloadedStrings #-}

-- | Typed git verbs — re-exports the 'Git' effect helpers from 'Tidepool.Effects'.
--
-- The same helpers (@gitLog@, @gitStatus@, @gitDiffStat@, @gitShow@) are
-- auto-generated into @Tidepool.Effects@ by the server and available without
-- this import.  Import this module explicitly when your code is in a
-- @.tidepool/lib/@ module that needs the typed names without pulling in the
-- full eval preamble.
--
-- Records ('Commit', 'StatusEntry', 'FileDelta') live in 'Tidepool.Records'
-- and are re-exported from 'Tidepool.Prelude'.  All parsing of porcelain
-- output and machine formats happens Rust-side; Haskell sees plain records.
module Tidepool.Git
  ( gitLog
  , gitStatus
  , gitDiffStat
  , gitShow
  ) where

import Tidepool.Effects (gitLog, gitStatus, gitDiffStat, gitShow)
