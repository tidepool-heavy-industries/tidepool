{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.RecipeBoundaryChecks (nestedCompilation, unboundAcrossRestart) where

import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check

nestedCompilation :: Member RecipeCheck effects => Eff effects ()
nestedCompilation = do
  actor <- root
  awaitCell actor "recipe execution admits nested actor compilation after preparation closes"
    "pure ((21 * 2 :: Int) == 42)"

unboundAcrossRestart :: Member RecipeCheck effects => Eff effects ()
unboundAcrossRestart = do
  before <- root
  awaitCell before "initial recipe reader reports its unbound conversation"
    "do { seen <- reflect 1; pure (case seen of { Left ReflectUnbound -> True; _ -> False }) }"
  identity <- restart
  check "restart retains an issued source selection" (not (Text.null identity))
  after <- root
  check "restart issues a root in the new swarm" (after /= before)
  awaitCell after "restarted recipe reader reports its unbound conversation"
    "do { seen <- reflect 1; pure (case seen of { Left ReflectUnbound -> True; _ -> False }) }"
