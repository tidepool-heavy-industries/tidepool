{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}

-- Executable examples, run against this candidate package without any models.
-- The driver does not know the project roles or order: this ordinary Haskell does.
module Project.Checks (script, checkImprovementSelection) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Check

script :: Member RecipeCheck effects => CheckActor -> Text -> Eff effects ()
script actor name = readFile actor (".exomonad/workspace/checks/" <> name <> ".hs") >>= void . turn actor

-- The selected RSI actor uses the configured model and receives current source.
checkImprovementSelection :: Member RecipeCheck effects => Eff effects ()
checkImprovementSelection = do
  improver <- activation
  check "requested RSI is an ordinary selected Astra" (checkModel improver == Just "gpt-6-astra" && "Current definitions:" `Text.isInfixOf` checkContext improver)
