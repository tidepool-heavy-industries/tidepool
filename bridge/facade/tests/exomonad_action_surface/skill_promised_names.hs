{-# LANGUAGE OverloadedStrings #-}

-- `renderGitOid` is used in worked examples in exomonad-orchestrate, exomonad-fork and
-- exomonad-unfold, and named in the `doc tree` topic, and exomonad-orchestrate lists
-- it under "Shipped — in every Exomonad cell". It was imported into the facade and
-- never re-exported, so a cell could not call it. A lead looking for it got a
-- correct `no match`, concluded discovery was broken, and pasted a hex string it
-- had read from a shell command eight minutes earlier.
module SkillPromisedNames where

import Data.Text (Text)
import Prelude
import qualified Tidepool.Actors.Exomonad as Exomonad

-- exomonad-unfold: `renderGitOid submitted`.
-- exomonad-fork:   `atRef (GitRef (renderGitOid commit))`.
oidText :: Exomonad.GitOid -> Text
oidText = Exomonad.renderGitOid

result :: Text
result = oidText (Exomonad.GitOid "0000000000000000000000000000000000000000")
