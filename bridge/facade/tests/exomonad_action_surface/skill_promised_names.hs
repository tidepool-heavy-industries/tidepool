{-# LANGUAGE OverloadedStrings #-}

module SkillPromisedNames where

import Data.Text (Text)
import Prelude
import qualified Tidepool.Actors.Exomonad as Exomonad

oidText :: Exomonad.GitOid -> Text
oidText = Exomonad.renderGitOid

result :: Text
result = oidText ("0000000000000000000000000000000000000000" :: Exomonad.GitOid)
