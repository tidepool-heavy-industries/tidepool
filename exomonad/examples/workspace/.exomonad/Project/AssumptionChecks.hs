{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.AssumptionChecks (changes) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Tidepool.Check

changes :: Member RecipeCheck effects => Eff effects ()
changes = do
  owner <- root
  void $ turn owner "import Project.AssumptionExamples"
  void $ turn owner "(producer, updates) <- unfoldDeferred (batch (\"assumption-design\" :: CampaignLabel) (\"checks\" :: ForkGroupLabel)) (childWithProgress @Int @Text (withLifetime ActorOwned (coding projectHead (assignment [label|producer|] (\"report observed build failures\" :: Text)))))"
  producer <- activation
  void $ turn owner "let project observation = case observation of { ProgressUpdate _ value -> Just value; _ -> Nothing }\nwatcher <- watchAssumption me (0 :: Int) (R.progress updates) project (pure . regression id \"new build failures: revisit the pending work\")"
  void $ turn (checkActor producer) "reportProgress (2 :: Int)"
  awaitCell owner "a specialized policy observes and reports a typed regression"
    "do { state <- R.call (assumptionView (R.client watcher)) (); pure (assumptionCurrent state == 2 && assumptionChangeCount state == 1 && case assumptionLastChange state of { Just change -> case changeDecision change of { ReportChange _ -> True; _ -> False }; Nothing -> False }) }"
  void $ turn owner "state <- R.call (assumptionView (R.client watcher)) ()"
  assertCell owner "text: regression report retains the supplied message"
    "case assumptionLastChange state of { Just change -> case changeDecision change of { ReportChange message -> message == \"new build failures: revisit the pending work\"; _ -> False }; Nothing -> False }"
  void $ turn (checkActor producer) "reportProgress (2 :: Int)"
  void $ turn (checkActor producer) "reportProgress (1 :: Int)"
  awaitCell owner "equal values skip policy; ignored changes retain their decision"
    "do { state <- R.call (assumptionView (R.client watcher)) (); pure (assumptionCurrent state == 1 && assumptionChangeCount state == 2 && case map changeDecision (assumptionRecentChanges state) of { [IgnoreChange _, ReportChange _] -> True; _ -> False }) }"
  void $ turn owner "state <- R.call (assumptionView (R.client watcher)) ()"
  assertCell owner "text: ignored and reported changes retain their messages"
    "case map changeDecision (assumptionRecentChanges state) of { [IgnoreChange ignored, ReportChange reported] -> ignored == \"the observed measure did not increase\" && reported == \"new build failures: revisit the pending work\"; _ -> False }"
  void $ turn owner "unresolved <- watchAssumption me (0 :: Int) (R.progress updates) project (const (pure (UnresolvedChange \"missing task context\")))"
  awaitCell owner "late attachment retains unresolved decision and mock notification refusal"
    "do { state <- R.call (assumptionView (R.client unresolved)) (); pure (case assumptionLastChange state of { Just change -> (case changeDecision change of { UnresolvedChange _ -> True; _ -> False }) && (case changeNotice change of { Just (Left NotificationUnavailable) -> True; _ -> False }); Nothing -> False }) }"
  void $ turn owner "state <- R.call (assumptionView (R.client unresolved)) ()"
  assertCell owner "text: unresolved change retains the supplied context"
    "case assumptionLastChange state of { Just change -> case changeDecision change of { UnresolvedChange context -> context == \"missing task context\"; _ -> False }; Nothing -> False }"
  void $ turn owner "R.finish watcher\nR.finish unresolved"
