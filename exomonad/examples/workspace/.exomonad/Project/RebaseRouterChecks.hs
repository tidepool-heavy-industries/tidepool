{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.RebaseRouterChecks (agentRef, facts) where

import Prelude hiding (readFile)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Set as Set
import qualified Data.Text as Text
import Tidepool.Check
import Project.Checks (checkSource)
import Project.RebaseRouter

facts :: Member RecipeCheck effects => Eff effects ()
facts = do
  let advance = ["src/shared.hs", "README.md"]
      overlap = exactFacts True advance (Set.fromList ["src/shared.hs", "src/child.hs"])
      disjoint = exactFacts True advance (Set.fromList ["src/child.hs"])
      current = exactFacts False advance (Set.fromList ["src/shared.hs"])
  check "exact path facts isolate overlap with the integration advance"
    (factsBehind overlap && factsOverlap overlap == Set.singleton "src/shared.hs"
      && factsBehind disjoint && Set.null (factsOverlap disjoint))
  check "the router sends on exact overlap, defers the ambiguous middle, and stays quiet when current"
    (adviceFor overlap 0.1 == SendNudge
      && adviceFor disjoint 0.5 == DeferJudgment
      && adviceFor disjoint 0.1 == RecordOnly
      && adviceFor current 0.99 == RecordOnly)

agentRef :: Member RecipeCheck effects => Eff effects ()
agentRef = do
  owner <- root
  source <- readFile owner (checkSource "rebase-router-agentref")
  result <- turn owner source
  check "an admission receipt resolves to a usable child AgentRef"
    ("Right ()" `Text.isInfixOf` output result)
