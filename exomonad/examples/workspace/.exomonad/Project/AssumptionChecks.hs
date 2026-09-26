{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.AssumptionChecks (changes) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check

changes :: Member RecipeCheck effects => Eff effects ()
changes = do
  owner <- root
  void $ turn owner "import Project.AssumptionExamples"
  void $ turn owner "(producer, updates) <- unfold (batch (\"assumption-design\" :: CampaignLabel) (\"checks\" :: ForkGroupLabel)) (childWithProgress @Int @Text (coding projectHead (assignment [label|producer|] (\"report observed build failures\" :: Text))))"
  producer <- activation
  void $ turn owner "let project observation = case observation of { ProgressUpdate _ value -> Just value; _ -> Nothing }\nwatcher <- watchAssumption me (0 :: Int) (R.progress updates) project (pure . regression id \"new build failures: revisit the pending work\")"
  void $ turn (checkActor producer) "reportProgress (2 :: Int)"
  first <- awaitOutput owner "state <- R.call (assumptionView (R.client watcher)) ()\n(assumptionCurrent state == 2, assumptionChangeCount state == 1, fmap (changeDecision) (assumptionLastChange state) == Just (ReportChange \"new build failures: revisit the pending work\"))" (Text.isInfixOf "(True, True, True)")
  check "a specialized policy observes and reports a typed regression" ("(True, True, True)" `Text.isInfixOf` first)
  void $ turn (checkActor producer) "reportProgress (2 :: Int)"
  void $ turn (checkActor producer) "reportProgress (1 :: Int)"
  second <- awaitOutput owner "state <- R.call (assumptionView (R.client watcher)) ()\n(assumptionCurrent state == 1, assumptionChangeCount state == 2, map changeDecision (assumptionRecentChanges state) == [IgnoreChange \"the observed measure did not increase\", ReportChange \"new build failures: revisit the pending work\"])" (Text.isInfixOf "(True, True, True)")
  check "equal values skip policy; ignored changes retain their decision" ("(True, True, True)" `Text.isInfixOf` second)
  void $ turn owner "unresolved <- watchAssumption me (0 :: Int) (R.progress updates) project (const (pure (UnresolvedChange \"missing task context\")))"
  pending <- awaitOutput owner "state <- R.call (assumptionView (R.client unresolved)) ()\ncase assumptionLastChange state of { Just change -> (changeDecision change == UnresolvedChange \"missing task context\", case changeNotice change of { Just (Left NotificationUnavailable) -> True; _ -> False }); Nothing -> (False,False) }" (Text.isInfixOf "(True, True)")
  check "late attachment retains unresolved decision and mock notification refusal" ("(True, True)" `Text.isInfixOf` pending)
  void $ turn owner "R.finish watcher\nR.finish unresolved"
