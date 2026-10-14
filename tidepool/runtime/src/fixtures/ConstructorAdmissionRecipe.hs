{-# LANGUAGE DataKinds #-}
{-# LANGUAGE PackageImports #-}
module ConstructorAdmissionRecipe where

import Control.Monad.Freer (Eff)
import "tidepool-resume" Tidepool.Internal.Resume (Settled, settle, resumeLifted)
import Tidepool.Check (RecipeCheck, root)

__prepared :: Settled '[RecipeCheck] Int
__prepared = settle (root >> pure 42 :: Eff '[RecipeCheck] Int)

__resume q x = settle (resumeLifted q x)
