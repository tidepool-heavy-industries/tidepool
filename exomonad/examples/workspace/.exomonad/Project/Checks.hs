{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}

-- Executable examples, run against this candidate package without any models.
-- The driver does not know the project roles or order: this ordinary Haskell does.
module Project.Checks (script, checkSource, checkImprovementSelection) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Exomonad.Workspace (workspaceRoot)
import Tidepool.Check

-- Every recipe fixture lives under the authored workspace's `checks/`
-- directory, wherever that checkout is mounted in this project (the
-- submodule layout's `.exomonad/workspace`, or the template layout's
-- `.exomonad`). Recipe sites build the path from `workspaceRoot` instead of
-- hardcoding either layout.
checkSource :: Text -> Text
checkSource name = Text.pack workspaceRoot <> "/checks/" <> name <> ".hs"

script :: Member RecipeCheck effects => CheckActor -> Text -> Eff effects ()
script actor name = readFile actor (checkSource name) >>= void . turn actor

-- The selected RSI actor uses the configured model and receives current source.
checkImprovementSelection :: Member RecipeCheck effects => Eff effects ()
checkImprovementSelection = do
  improver <- activation
  check "requested RSI is an ordinary selected Astra" (checkModel improver == Just "gpt-6-astra" && "Current definitions:" `Text.isInfixOf` checkContext improver)
