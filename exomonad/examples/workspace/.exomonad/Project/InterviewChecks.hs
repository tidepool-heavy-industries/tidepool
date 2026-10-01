{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.InterviewChecks (collectAnswers) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Tidepool.Check
import Project.Checks (script)

collectAnswers :: Member RecipeCheck effects => Eff effects ()
collectAnswers = do
  owner <- root
  source <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral source)
  script owner "interview-collect"
  assertCell owner "an empty interview cannot claim completion" "not (interviewComplete (InterviewReport []))"
  void $ turn owner "pending <- collectInterview interviewItems"
  assertCell owner "supplied live answer is explicitly incomplete" "not (interviewComplete pending) && case interviewFindings pending of { [Waiting _ AnswerWorking] -> True; _ -> False }"
  expert <- activation
  void $ turn (checkActor expert)
    "respond (Decision \"choose one source\" [\"inspected exact input\"] :: DesignAnswer)"
  void $ turn owner "answered <- collectInterview interviewItems"
  assertCell owner "terminal typed answer appears in one summary" "interviewComplete answered && case interviewFindings answered of { [Answered _ receipt] -> responseValue receipt == Decision \"choose one source\" [\"inspected exact input\"]; _ -> False }"
  assertCell owner "text: interview summary includes the terminal answer" "\"choose one source\" `T.isInfixOf` interviewSummary answered"
  void $ turn owner "let prior = case interviewFindings answered of { [Answered q receipt] -> [KnownAnswer q receipt]; _ -> [] }\nreused <- collectInterview prior"
  assertCell owner "earlier answer is reused with its receipt" "interviewComplete reused && case (interviewFindings answered, interviewFindings reused) of { ([Answered originalQuestion originalReceipt], [Answered question receipt]) -> question == originalQuestion && receipt == originalReceipt; _ -> False }"
