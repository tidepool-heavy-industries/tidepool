{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}

-- | A workspace whose only configured module is pinned through the project's
-- flake. The cell below is an ordinary prepared turn in the resident root
-- actor, so its answer proves the external source reached the running program
-- rather than merely the capture directory.
module Project.Checks (pinned) where

import Control.Monad.Freer (Eff, Member)
import Tidepool.Check

pinned :: Member RecipeCheck effects => Eff effects ()
pinned = do
  owner <- root
  assertCell owner "a prepared cell evaluates the flake-pinned module"
    "tiny + 1 == (42 :: Int)"
