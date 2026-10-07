{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.QuantitiesChecks (workspaceProfile) where

import Prelude hiding (readFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Exomonad.Workspace (workspaceRoot)
import Tidepool.Check

-- Use the actual authored workspace and its installed spec, including the
-- capture owner's generated workspace metadata and pinned Jev source.
workspaceProfile :: Member RecipeCheck effects => Eff effects ()
workspaceProfile = do
  owner <- root
  source <- readFile owner (Text.pack workspaceRoot <> "/checks/workspace-quantities.hs")
  void (turn owner source)
  assertCell owner "the installed workspace notebooks retain scope and sleep"
    "not (null workspaceProfileKeys) && all (\\keys -> \"ResourceScopes\" `elem` keys && \"Sleep\" `elem` keys) workspaceProfileKeys"
  assertCell owner "WorkspaceEffects executes scoped sleep with confirmed cleanup"
    "scopeBody workspaceSleepOutcome == Right () && scopeCleanup workspaceSleepOutcome == Right ()"
