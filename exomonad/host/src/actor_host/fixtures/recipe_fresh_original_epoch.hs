{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.EpochStartupChecks (installedOriginalEpoch) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Tidepool.Check

installedOriginalEpoch :: Member RecipeCheck effects => Eff effects ()
installedOriginalEpoch = do
  owner <- root
  void $ turn owner
    "import Data.Text (Text)\nimport Tidepool.Actors.Exomonad\nimport Project.Work (workspaceAgentSpec)\nRight worker <- spawnSubagent (FreshCtx \"Answer the typed request.\") SameDir ((defaultSpawnOptions workspaceAgentSpec) { spawnModel = Just (OpenAIModel Gpt6Luna) })"
  void $ turn owner
    "Right pending <- request @Text worker (\"epoch request\" :: Text) defaultRequestOptions"
  worker <- activation
  void $ turn (checkActor worker) "import Data.Text (Text)"
  assertCell (checkActor worker)
    "the installed notebook receives after source bootstrap publication"
    "(sessionInput :: Text) == \"epoch request\""
  void $ turn (checkActor worker) "respond (\"epoch reply\" :: Text)"
  void $ turn owner "Right reply <- await (result pending)"
  assertCell owner "the exact typed request returns its worker reply"
    "reply == \"epoch reply\""
