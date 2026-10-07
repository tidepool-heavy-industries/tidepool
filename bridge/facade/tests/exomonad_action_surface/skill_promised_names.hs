{-# LANGUAGE OverloadedStrings #-}

-- Exact-candidate project review guidance renders Git identities through the
-- public facade; authored cells can call the same exported formatter.
module SkillPromisedNames where

import Data.Text (Text)
import Prelude
import qualified Tidepool.Actors.Exomonad as Exomonad

oidText :: Exomonad.GitOid -> Text
oidText = Exomonad.renderGitOid

result :: Text
result = oidText (Exomonad.GitOid "0000000000000000000000000000000000000000")
