{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.RecursiveWorkChecks (nestedRequests, revisedReview) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check
import Project.Checks (script)

-- Scripted model replies exercise explicit idle spawn, raw typed requests and
-- ordinary progress/result collection at three ownership levels.
-- Findings stay findings throughout three levels; scaffold commits test source
-- inheritance and never masquerade as Candidate results.
nestedRequests :: Member RecipeCheck effects => Eff effects ()
nestedRequests = do
  owner <- root
  base <- git owner ["rev-parse", "HEAD"]
  setup owner base "first"
  component <- activation
  sibling <- activation
  void $ turn owner "early <- readWork collection"
  assertCell owner "collector retains both pending caller-owned requests" "length (collectedWork early) == 2 && all ((== Nothing) . sourceResult) (collectedWork early)"
  componentSource <- checkpoint (checkActor component) "component-scaffold.txt" "component contract\n" "component scaffold fixture"
  setup (checkActor component) componentSource "subcomponents"
  subcomponent <- activation
  peer <- activation
  inheritedComponent <- git (checkActor subcomponent) ["rev-parse", "HEAD"]
  check "subcomponent starts from its local owner scaffold, not root HEAD" (inheritedComponent == componentSource && componentSource /= base)
  subcomponentSource <- checkpoint (checkActor subcomponent) "subcomponent-scaffold.txt" "microtask contract\n" "subcomponent scaffold fixture"
  setup (checkActor subcomponent) subcomponentSource "microtasks"
  leaf1 <- activation
  leaf2 <- activation
  inheritedSubcomponent <- git (checkActor leaf1) ["rev-parse", "HEAD"]
  check "microtask starts from the next local scaffold" (inheritedSubcomponent == subcomponentSource && subcomponentSource /= componentSource)
  void $ turn (checkActor leaf1) "respond (Produced (\"left evidence\" :: Text))"
  void $ turn (checkActor leaf2) "respond (Produced (\"right evidence\" :: Text))"
  finish (checkActor subcomponent)
  void $ turn (checkActor subcomponent) "respond (Produced (\"joined microtasks\" :: Text))"
  void $ turn (checkActor peer) "respond (Produced (\"independent evidence\" :: Text))"
  finish (checkActor component)
  -- A second local set of requests is created only after the first settles.
  setup (checkActor component) componentSource "followup"
  later1 <- activation
  later2 <- activation
  void $ turn (checkActor later1) "respond (Produced (\"followup one\" :: Text))"
  void $ turn (checkActor later2) "respond (Produced (\"followup two\" :: Text))"
  finish (checkActor component)
  void $ turn (checkActor component) "respond (Produced (\"component findings\" :: Text))"
  void $ turn (checkActor sibling) "respond (Produced (\"sibling findings\" :: Text))"
  finish owner
  where
    setup actor base phase = do
      void $ turn actor ("let sourceHead = " <> gitOidLiteral base <> "\nlet phaseName = " <> Text.pack (show phase) <> " :: Text")
      script actor "recursive-requests"
    finish actor = do
      awaitCell actor "both original sources have terminal results"
        "do { state <- readWork collection; pure (length (filter (maybe False (const True) . sourceResult) (collectedWork state)) == 2) }"
      void $ turn actor "import qualified Tidepool.Actor as Actor\nclosed <- finishWork collection"
      assertCell actor "settled findings drain without retiring their workers"
        "case closed of { Actor.Completed _ -> True; _ -> False }"

-- A new review request changes both the exact source and the actor, while
-- preserving the original scope. It must not disturb the previous checkout.
revisedReview :: Member RecipeCheck effects => Eff effects ()
revisedReview = do
  owner <- root
  base <- git owner ["rev-parse", "HEAD"]
  first <- checkpoint owner "candidate.txt" "first\n" "first review candidate"
  void $ turn owner ("let basis = ExactScope " <> gitOidLiteral base <> " [\"candidate.txt\"] \"The candidate contains the corrected text\"\nlet original = ReviewRequest basis (Candidate " <> gitOidLiteral first <> " [] []) OwnerRepairs\nRight (review, progress) <- requestReview \"first-review\" original")
  reviewer <- activation
  firstHead <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "initial review uses Luna at the exact candidate"
    (firstHead == first && checkModel reviewer == Just "gpt-6-luna")
  void $ turn (checkActor reviewer) "respond (Produced (Repair (reviewInput sessionInput) [\"correct the text\"]))"
  revised <- checkpoint owner "candidate.txt" "corrected\n" "revised review candidate"
  void $ turn owner ("let revised = original { reviewInput = Candidate " <> gitOidLiteral revised <> " [] [] }\nRight (nextReview, nextProgress) <- requestReview \"revised-review\" revised")
  next <- activation
  nextHead <- git (checkActor next) ["rev-parse", "HEAD"]
  oldHead <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "revised review gets a fresh actor and the new candidate"
    (checkActor next /= checkActor reviewer && nextHead == revised)
  check "revised review leaves the retained previous checkout intact" (oldHead == first)
  assertCell (checkActor next) "fresh review preserves the original cumulative scope"
    ("reviewBase (reviewBasis sessionInput) == " <> gitOidLiteral base <> " && reviewOwnedPaths (reviewBasis sessionInput) == [\"candidate.txt\"]")
