{-# LANGUAGE QuasiQuotes #-}
import qualified Project.DecisionAnswers as Answers
import qualified Tidepool.Actor.Record as R
let question = Question "cancel" (DesignQuestion "plans/component.md" sourceHead "Should cancelling a wait kill the job?" [] [] ["wait handling"])
let decision = AcceptedDecision question sourceHead "Cancelling a wait detaches that waiter; it does not terminate the job" ["plans/component.md#cancellation"]
let work = (task "wait" "Implement waiter cancellation" ["src/wait.rs"] "Job stays observable after waiter cancellation" sourceHead) { planPath = "plans/component.md", acceptedDecisions = [decision] }
Right answers <- Answers.startDecisionAnswersWith (\_ _ -> pure (Answers.UseDecision 0)) me [Answers.AnswerTarget "wait" me work]
