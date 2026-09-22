{-# LANGUAGE OverloadedStrings #-}

-- `renderGitOid` is used in worked examples in shoal-orchestrate, shoal-fork and
-- shoal-unfold, and named in the `doc tree` topic, and shoal-orchestrate lists
-- it under "Shipped — in every Shoal cell". It was imported into the facade and
-- never re-exported, so a cell could not call it. A lead looking for it got a
-- correct `no match`, concluded discovery was broken, and pasted a hex string it
-- had read from a shell command eight minutes earlier.
module SkillPromisedNames where

import Data.Text (Text)
import Prelude
import qualified Tidepool.Actors.Shoal as Shoal

-- shoal-unfold: `renderGitOid submitted`.
-- shoal-fork:   `atRef (GitRef (renderGitOid commit))`.
oidText :: Shoal.GitOid -> Text
oidText = Shoal.renderGitOid

result :: Text
result = oidText (Shoal.GitOid "0000000000000000000000000000000000000000")
