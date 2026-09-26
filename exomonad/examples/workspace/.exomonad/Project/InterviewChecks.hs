{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.InterviewChecks (collectAnswers) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check
import Project.Checks (script)

collectAnswers :: Member RecipeCheck effects => Eff effects ()
collectAnswers = do
  owner <- root
  source <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral source)
  script owner "interview-collect"
  empty <- turn owner "inspectFull (interviewSummary (InterviewReport []))"
  check "an empty interview cannot claim completion"
    ("Interview incomplete: no questions supplied" `Text.isInfixOf` output empty)
  pending <- turn owner
    "pending <- collectInterview interviewItems\ninspectFull (interviewSummary pending)"
  check "supplied live answer is explicitly incomplete"
    ("Interview incomplete" `Text.isInfixOf` output pending
      && "waiting:" `Text.isInfixOf` output pending)
  expert <- activation
  void $ turn (checkActor expert)
    "respond (Decision \"choose one source\" [\"inspected exact input\"] :: DesignAnswer)"
  answered <- turn owner
    "answered <- collectInterview interviewItems\ninspectFull (interviewSummary answered)"
  check "terminal typed answer appears in one summary"
    ("Interview complete" `Text.isInfixOf` output answered
      && "choose one source" `Text.isInfixOf` output answered)
  reused <- turn owner
    "let prior = case interviewFindings answered of { [Answered q receipt] -> [KnownAnswer q receipt]; _ -> [] }\nreused <- collectInterview prior\ninspectFull (interviewSummary reused)"
  check "earlier answer is reused with its receipt"
    ("Interview complete" `Text.isInfixOf` output reused
      && "choose one source" `Text.isInfixOf` output reused)
