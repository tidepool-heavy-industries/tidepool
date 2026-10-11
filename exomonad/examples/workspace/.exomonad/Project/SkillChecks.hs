{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.SkillChecks
  ( skills
  , lunaApplicativeAwait
  , notebookForms
  , reviewProvenance
  ) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Exomonad.Workspace (workspaceRoot)
import Tidepool.Check
import Tidepool.Aeson (Value)
import Project.Work (workspaceAgentSpec)

-- Execute the skill's actual code blocks, not separately maintained copies.
exampleBody :: Member RecipeCheck effects => CheckActor -> Text -> Int -> Eff effects Text
exampleBody actor skill index = do
  body <- readFile actor (Text.pack workspaceRoot <> "/skills/" <> skill <> "/SKILL.md")
  let blocks = map (fst . Text.breakOn "```") (drop 1 (Text.splitOn "```haskell\n" body))
  pure (blocks !! index)

example :: Member RecipeCheck effects => CheckActor -> Text -> Int -> Eff effects Value
example actor skill index = exampleBody actor skill index >>= turn actor

lunaApplicativeAwait :: Member RecipeCheck effects => Eff effects ()
lunaApplicativeAwait = do
  owner <- root
  let fixture path = readFile owner (Text.pack workspaceRoot <> "/checks/" <> path)
  start <- fixture "luna-agent-start.hs"
  finish <- fixture "luna-agent-settlement.hs"
  void $ turn owner start
  first <- activation
  second <- activation
  let (compiler, lookupWorker) =
        if checkLabel first == "compiler-scout" then (first, second) else (second, first)
  check "the two requests activate their named workers"
    (checkLabel compiler == "compiler-scout" && checkLabel lookupWorker == "lookup-scout")
  check "the concrete workspace recipe selects Luna for the compiler scout"
    (checkModel compiler == Just "gpt-6-luna")
  check "the concrete workspace recipe selects Luna for the lookup scout"
    (checkModel lookupWorker == Just "gpt-6-luna")
  assertCell (checkActor compiler) "the compiler scout receives its typed request"
    "(sessionInput :: Text) == \"Summarize the named compiler boundary in one sentence.\""
  assertCell (checkActor lookupWorker) "the lookup scout receives its typed request"
    "(sessionInput :: Text) == \"Summarize the public lookup contract in one sentence.\""
  void $ turn (checkActor compiler)
    "respond (\"compiler note: \" <> (sessionInput :: Text))"
  void $ turn (checkActor lookupWorker)
    "respond (\"lookup note: \" <> (sessionInput :: Text))"
  void $ turn owner finish
  assertCell owner "one applicative await retains both typed settlements"
    "case joinedScoutSettlements of { Right (Right compiler, Right lookup) -> responseValue compiler == \"compiler note: Summarize the named compiler boundary in one sentence.\" && responseValue lookup == \"lookup note: Summarize the public lookup contract in one sentence.\"; _ -> False }"

skills :: Member RecipeCheck effects => Eff effects ()
skills = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner "let privateSkillNotebook = True\nlet input = \"typed skill request\" :: Text\nRight worker <- spawnSubagent (FreshCtx \"Reply to one typed request.\") SameDir ((defaultSpawnOptions workspaceAgentSpec) { spawnModel = Just (Alias \"luna\"), spawnEffort = Just Medium, spawnInstructions = Just (projectPrompt \"task\"), spawnLabel = Just \"skill-example\" })"
  requestExample <- exampleBody owner "exomonad-agent-work" 0
  -- The model-free recipe drives the reply after request admission, before
  -- executing the published example's blocking await.
  let (requestSource, replySource) = Text.breakOn "\n" requestExample
  void $ turn owner requestSource
  worker <- activation
  check "typed request activates the explicitly spawned Luna worker" (checkModel worker == Just "gpt-6-luna")
  refuseCell (checkActor worker) "fresh skill worker does not inherit parent notebook declarations"
    "privateSkillNotebook"
  assertCell (checkActor worker) "the worker receives the typed skill request"
    "(sessionInput :: Text) == \"typed skill request\""
  void $ turn (checkActor worker)
    "import qualified Tidepool.Agent.Reply as SkillReply\nfirstScope <- (SkillReply.currentRequest :: Eff WorkspaceEffects (SkillReply.RequestScope Text Text))\nlet firstSkillRequest = case firstScope of { SkillReply.RequestActive request _ -> SkillReply.requestIdNumber request; _ -> -1 }"
  assertCell (checkActor worker) "the first skill request has its own active typed identity"
    "case firstScope of { SkillReply.RequestActive _ active -> active == (sessionInput :: Text); _ -> False }"
  void $ turn (checkActor worker) "respond (\"typed result\" :: Text)"
  void $ turn owner (Text.drop 1 replySource)
  assertCell owner "the published skill awaits the exact request result" "reply == \"typed result\""
  void $ turn owner "import Tidepool.Agent.Reply (ExecutionReceipt (..), responseExecution)\nimport qualified Tidepool.Actor as Actor\nfirstSkillReceipt <- pollResponse pending"
  assertCell owner "the first skill receipt belongs to the original request"
    "case firstSkillReceipt of { ResponseReady receipt -> executionRequest (responseExecution receipt) == requestId pending && responseValue receipt == \"typed result\"; _ -> False }"
  void $ example owner "exomonad-define-actors" 0
  awaitCell owner "record skill joins differently typed inputs"
    "(== Just (\"abc123\",4)) <$> R.call (joined endpoints) ()"
  void $ turn owner "let resultsWorker = worker\nRight worker <- request @(Outcome Candidate) resultsWorker input defaultRequestOptions"
  let resultsWorker = worker
  void $ turn owner "candidateAdmission <- pollResponse worker\ndisplay (T.pack (show (requestId pending, requestId worker, candidateAdmission)))"
  assertCell owner "the new skill request has a distinct identity without an observed terminal refusal"
    "case candidateAdmission of { ResponseUnavailable failure -> error (T.pack (show (requestId pending, requestId worker, failure))); _ -> requestId pending /= requestId worker }"
  reused <- activation
  check "the heterogeneous request activates the same actor incarnation"
    (checkActor reused == checkActor resultsWorker)
  awaitCellBy (checkActor resultsWorker) "the retained actor mounts the new skill request and reply type"
    "\\scope -> case scope of { Right (request, active) -> SkillReply.requestIdNumber request /= firstSkillRequest && active == \"typed skill request\"; _ -> False }"
    "do { scope <- (SkillReply.currentRequest :: Eff WorkspaceEffects (SkillReply.RequestScope Text (Outcome Candidate))); pure (case scope of { SkillReply.RequestActive request active -> Right (request, active); SkillReply.RequestUnavailable failure -> Left failure }) }"
  void $ turn (checkActor resultsWorker)
    "candidateScope <- (SkillReply.currentRequest :: Eff WorkspaceEffects (SkillReply.RequestScope Text (Outcome Candidate)))\noldResultScope <- (SkillReply.currentRequest :: Eff WorkspaceEffects (SkillReply.RequestScope Text Text))\ndisplay (T.pack (show (case candidateScope of { SkillReply.RequestActive request active -> Right (request, active); SkillReply.RequestUnavailable failure -> Left failure })))"
  assertCell (checkActor resultsWorker) "the old result type no longer borrows the active request"
    "case oldResultScope of { SkillReply.RequestUnavailable SkillReply.RequestTypeMismatch -> True; _ -> False }"
  void $ example owner "exomonad-define-actors" 1
  void $ turn owner "candidateBeforeReply <- pollResponse worker\nresultsBeforeReply <- R.call (resultCount (R.client results)) ()\ndisplay (T.pack (show (candidateBeforeReply, resultsBeforeReply)))"
  assertCell owner "the exact candidate request remains available for its deliberate reply"
    "case candidateBeforeReply of { ResponseStarting _ -> True; ResponsePending _ -> True; _ -> False }"
  void $ turn (checkActor resultsWorker) "respond (Blocked \"skill result\" [] :: Outcome Candidate)"
  awaitCellBy owner "the exact skill request reaches successful typed settlement"
    "\\observed -> case observed of { ResponseReady _ -> True; ResponseUnavailable failure -> error (T.pack (show failure)); _ -> False }"
    "pollResponse worker"
  awaitCellBy owner "record skill observes the real request settlement"
    "(== 1)" "R.call (resultCount (R.client results)) ()"
  void $ turn owner "candidateReceipt <- pollResponse worker\nresultsExit <- R.finish results\nfirstSkillAfterReuse <- pollResponse pending"
  assertCell owner "record settlement retains the exact new request, value and actor incarnation"
    "case (firstSkillReceipt, candidateReceipt, resultsExit) of { (ResponseReady original, ResponseReady receipt, Actor.Completed [Right event]) -> executionRequest (responseExecution receipt) == requestId worker && responseValue receipt == Blocked \"skill result\" [] && event == receipt && executionActorId (responseExecution receipt) == executionActorId (responseExecution original) && executionActorIncarnation (responseExecution receipt) == executionActorIncarnation (responseExecution original); _ -> False }"
  assertCell owner "reusing an actor preserves the original request receipt"
    "firstSkillAfterReuse == firstSkillReceipt && requestId pending /= requestId worker"

  -- Execute the published notebook forms in the actual ambient namespace.
  -- File/command examples belong to the command material.
  void $ example owner "exomonad-workbench" 0
  void $ example owner "exomonad-workbench" 1
  void $ example owner "exomonad-workbench" 2
  assertCell owner "text: the workbench skill renders an Int into Text with T.pack . show"
    "attempts == 3 && note == \"retry budget 3 exhausted\""
  -- Normal completion proves the annotated binding installs without forcing it.
  void $ example owner "exomonad-workbench" 3
  void $ example owner "exomonad-workbench" 4
  void $ example owner "exomonad-workbench" 6

  -- The Jev skill's packets must compile against the real operators and
  -- resolve to a typed value whether or not an endpoint is configured. Block 0
  -- gathers previews with commands; the packet that judges them is block 1.
  void $ turn owner "let previews = [(\"README.md\", \"# jev-dsl\\ntyped packets\"), (\"LICENSE\", \"MIT\")] :: [(Text, Text)]"
  void $ example owner "exomonad-jev" 1
  void $ example owner "exomonad-jev" 2
  assertCell owner "the review gate resolves to an accepted key or a stated doubt"
    "case answer of { Left _ -> True; Right response -> let a = J.answers response in case J.takenUnder J.careful a of { Left doubt -> not (T.null doubt.why); Right settled -> let verdict = J.settledValue settled in (a.key, verdict) `elem` [(\"all_present\", \"merge: every item of the checklist holds\"), (\"one_absent\", \"repair: an item of the checklist does not hold\"), (\"contradicts\", \"escalate: the artifacts contradict each other\"), (\"insufficient_evidence\", \"ask again: name the missing field and re-ask\")] } }"
  void $ example owner "exomonad-jev" 3
  assertCell owner "text: the selected continuation runs and returns its command output"
    "case answer of { Left _ -> next == \"jev unavailable; inspecting by hand\"; Right _ -> next `elem` [\"would rerun session::retry_is_bounded\\n\", \"would rerun the suite\\n\", \"reading the failure by hand\", \"rerun unavailable\", \"suite unavailable\"] }"
  void $ example owner "exomonad-jev" 4

  checkNotebookForms owner

-- Run the pure authoring forms independently of the worker/review examples.
notebookForms :: Member RecipeCheck effects => Eff effects ()
notebookForms = root >>= checkNotebookForms

checkNotebookForms :: Member RecipeCheck effects => CheckActor -> Eff effects ()
checkNotebookForms owner = do
  -- The project-free record definition: no Exomonad.Contrib.Actors wrapper, the row
  -- named in the cell and pinned by a signature on the definition.
  void $ example owner "exomonad-define-actors" 2
  awaitCell owner "a record definition pins its own effect row without a project wrapper"
    "(== 1) <$> R.call (noteCount (R.client tally)) ()"

  -- Execute the authoring forms, then inspect their actual typed bindings.
  void $ example owner "exomonad-workbench" 7
  assertCell owner "text: a signature beside its equation installs both forms of binding"
    "map severity [1,3] == [\"low\",\"high\"] && inline 7 == \"work 7\""
  void $ example owner "exomonad-workbench" 8
  assertCell owner "an annotated literal settles an otherwise ambiguous encodable field"
    "state == object [\"owned_path\" .= (\"src/app.rs\" :: Text), \"changed\" .= (2 :: Int)]"
  void $ example owner "exomonad-workbench" 9
  assertCell owner "branch and ref identities round-trip through their constructors"
    "(case onto of BranchName b -> b) == \"integration/tags\" && (case from of GitRef ref -> ref) == \"exomonad/integration\""

-- Exercise the published root review call with different base and candidate OIDs.
-- No model is launched: the recipe actor verifies the real admission packet.
reviewProvenance :: Member RecipeCheck effects => Eff effects ()
reviewProvenance = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  candidate <- checkpoint owner "review-provenance.txt" "review candidate\n" "review provenance fixture"
  void $ turn owner ("let base = " <> gitOidLiteral baseline <> "\nlet commit = " <> gitOidLiteral candidate)
  void $ turn owner ("let reviewInputValue = ReviewRequest (ExactScope " <> gitOidLiteral baseline
    <> " [\"review-provenance.txt\"] \"review provenance fixture\") (Candidate "
    <> gitOidLiteral candidate <> " [] []) OwnerRepairs\nRight (reviewRequest, reviewProgress) <- requestReview \"review-provenance\" reviewInputValue")
  reviewer <- activation
  actual <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "review checkout is the assigned candidate" (actual == candidate)
  void $ turn (checkActor reviewer) "let current = sessionInput :: ReviewRequest\nlet latest = reviewInput current"
  assertCell (checkActor reviewer) "typed request preserves both revision identities"
    ("reviewBase (reviewBasis current) == " <> gitOidLiteral baseline <> " && candidateCommit latest == " <> gitOidLiteral candidate)
  void $ turn (checkActor reviewer)
    "import qualified Tidepool.Agent.Reply as ReviewReply\noldScope <- (ReviewReply.currentRequest :: Eff WorkspaceEffects (ReviewReply.RequestScope ReviewRequest (Outcome ReviewDecision)))\nlet oldId = case oldScope of { ReviewReply.RequestActive request _ -> ReviewReply.requestIdNumber request; _ -> -1 }"
  assertCell (checkActor reviewer) "request scope borrows the exact active review input"
    ("case oldScope of { ReviewReply.RequestActive _ active -> candidateCommit (reviewInput active) == " <> gitOidLiteral candidate <> "; _ -> False }")
  void $ turn (checkActor reviewer)
    "wrongInput <- (ReviewReply.currentRequest :: Eff WorkspaceEffects (ReviewReply.RequestScope () (Outcome ReviewDecision)))\nwrongResult <- (ReviewReply.currentRequest :: Eff WorkspaceEffects (ReviewReply.RequestScope ReviewRequest ()))"
  assertCell (checkActor reviewer) "wrong request input and result types refuse before borrowing"
    "case (wrongInput, wrongResult) of { (ReviewReply.RequestUnavailable ReviewReply.RequestTypeMismatch, ReviewReply.RequestUnavailable ReviewReply.RequestTypeMismatch) -> True; _ -> False }"
  -- Named submit_review admission and refusal controls run through the actual
  -- provider tool consumer. This recipe owns typed response provenance.
  void $ turn (checkActor reviewer)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis current) latest [\"recipe verified candidate checkout\"] \"review provenance fixture\")))"
  void $ turn owner "accepted <- pollResponse reviewRequest"
  assertCell owner "exact acceptance retains its scope without fabricating a Task"
    "case accepted of { ResponseReady receipt -> case responseValue receipt of { Produced (Accepted checked) -> case reviewedBasis checked of { ExactScope originalBase _ _ -> originalBase == base && candidateCommit (reviewedCandidate checked) == commit; _ -> False }; _ -> False }; _ -> False }"
  -- Exact review retries retain the real scope, including when the caller offers
  -- a retained implementer: there is no implementation Task to assign to it.
  void $ turn owner "let exact = ReviewRequest (ExactScope base [\"review-provenance.txt\"] \"exact acceptance\") (Candidate commit [] [\"remaining gate\"]) (RetainedImplementer (responseActor reviewRequest))\nRight (retry, retryProgress) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewRequest) exact defaultRequestOptions"
  retryReviewer <- activation
  check "the exact retry activates the retained reviewer incarnation"
    (checkActor retryReviewer == checkActor reviewer)
  void $ turn (checkActor reviewer) "let current = sessionInput :: ReviewRequest\nlet latest = reviewInput current\nverdict <- repair \"exact-findings\" current latest [\"repair at the owner\"]"
  assertCell (checkActor reviewer) "exact-scope repair returns findings without inventing an implementation assignment"
    "case verdict of { Left (Repair found issues) -> found == latest && issues == [\"repair at the owner\"]; _ -> False }"
  void $ turn (checkActor reviewer) "respond (Produced (Repair latest [\"repair at the owner\"]))"
  revised <- checkpoint owner "review-provenance.txt" "revised review candidate\n" "review repair fixture"
  void $ git (checkActor reviewer) ["merge", "--ff-only", revised]
  void $ turn owner ("let revisedCommit = " <> gitOidLiteral revised)
  void $ turn owner "let question = Question \"contract\" (DesignQuestion \"plan.md\" base \"boundary\" [\"evidence\"] [] [])\nlet decision = AcceptedDecision question base \"preserve the boundary\" [\"checked\"]\nlet assigned = (task \"assigned-review\" \"review implementation\" [\"review-provenance.txt\"] \"assigned acceptance\" base) { planPath = \"plan.md\", rationale = \"retained reason\", acceptedDecisions = [decision] }\nlet assignedRequest = ReviewRequest (AssignedTask assigned) (Candidate revisedCommit [\"candidate check\"] [\"open gate\"]) OwnerRepairs\nRight (assignedReview, assignedProgress) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewRequest) assignedRequest defaultRequestOptions"
  assignedReviewer <- activation
  check "the assigned retry activates the retained reviewer incarnation"
    (checkActor assignedReviewer == checkActor reviewer)
  void $ turn (checkActor reviewer) "let current = sessionInput :: ReviewRequest\nlet latest = reviewInput current"
  void $ turn (checkActor reviewer)
    "newScope <- (ReviewReply.currentRequest :: Eff WorkspaceEffects (ReviewReply.RequestScope ReviewRequest (Outcome ReviewDecision)))\nlet newId = case newScope of { ReviewReply.RequestActive request _ -> ReviewReply.requestIdNumber request; _ -> -1 }"
  assertCell (checkActor reviewer) "a prepared retained request replaces identity and mounted input"
    ("case newScope of { ReviewReply.RequestActive request active -> ReviewReply.requestIdNumber request /= oldId && candidateCommit (reviewInput active) == " <> gitOidLiteral revised <> "; _ -> False }")
  void $ turn (checkActor reviewer)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis current) latest [\"checked revised candidate\"] \"assigned acceptance\")))"
  void $ turn owner "retained <- pollResponse assignedReview"
  assertCell owner "assigned acceptance preserves the whole Task, decisions and candidate gates"
    "case retained of { ResponseReady receipt -> case responseValue receipt of { Produced (Accepted checked) -> reviewedBasis checked == AssignedTask assigned && reviewedCandidate checked == reviewInput assignedRequest; _ -> False }; _ -> False }"
