{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.SkillChecks (skills, notebookForms, reviewProvenance) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.List (sortOn)
import Data.Text (Text)
import qualified Data.Text as Text
import Exomonad.Workspace (workspaceRoot)
import Tidepool.Check
import Tidepool.Aeson (Value)

-- Execute the skill's actual code blocks, not separately maintained copies.
example :: Member RecipeCheck effects => CheckActor -> Text -> Int -> Eff effects Value
example actor skill index = do
  body <- readFile actor (Text.pack workspaceRoot <> "/skills/" <> skill <> "/SKILL.md")
  let blocks = map (fst . Text.breakOn "```") (drop 1 (Text.splitOn "```haskell\n" body))
  turn actor (blocks !! index)

skills :: Member RecipeCheck effects => Eff effects ()
skills = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let campaign = \"skills\" :: CampaignLabel\nlet group = \"examples\" :: ForkGroupLabel\nlet work = Task (batch campaign group) \".exomonad/workspace/skills/exomonad-fork/SKILL.md\" " <> gitOidLiteral baseline <> " \"Exercise skill examples\" \"Check actual resident composition\" [] \"Typed result and progress\" []\nlet source = projectHead")
  void $ example owner "exomonad-fork" 0
  worker <- activation
  check "skill launches a fresh Luna Medium worker" (checkModel worker == Just "gpt-6-luna" && "Exercise skill examples" `Text.isInfixOf` checkContext worker)
  void $ turn owner "let Just group = forkGroupHandle worker\ncleanup <- releaseGroup group"
  assertCell owner "scoped release retains the pending worker instead of cancelling its request"
    "not (cleanupReceiptComplete cleanup) && any (\\step -> case step of { CleanupBlocked _ -> True; _ -> False }) (cleanupReceiptSteps cleanup)"
  void $ example owner "exomonad-define-actors" 0
  awaitCell owner "record skill joins differently typed inputs"
    "(== Just (\"abc123\",4)) <$> R.call (joined endpoints) ()"
  void $ example owner "exomonad-define-actors" 1
  awaitCell owner "record skill attaches the exact pending request"
    "(== 0) <$> R.call (resultCount (R.client results)) ()"
  void $ turn (checkActor worker) ("let candidate = Candidate " <> gitOidLiteral baseline <> " [\"example check\"] [\"product acceptance remains\"]")
  void $ example (checkActor worker) "exomonad-coordinate" 0
  void $ example owner "exomonad-coordinate" 1
  awaitCell owner "collector retains candidate progress and pending result"
    ("do { observed <- readWork router; pure (case collectedWork observed of { [source] -> case sourceResult source of { Nothing -> any (\\candidate -> candidateCommit candidate == " <> gitOidLiteral baseline <> " && remainingGates candidate == [\"product acceptance remains\"]) (workEvidence (sourceProgress source)); _ -> False }; _ -> False }) }")
  -- Block 0 is reviewCommit's own root recipe: run it from the actual root
  -- actor (owner), not a forked child, so a regression to a relative
  -- subgroup path (which only a child with an allocated parent path can
  -- resolve) fails this turn instead of passing unnoticed.
  void $ turn owner ("let base = " <> gitOidLiteral baseline <> "\nlet commit = " <> gitOidLiteral baseline)
  void $ example owner "exomonad-review" 0
  commitReviewer <- activation
  check "root review-by-commit forks a fresh Luna reviewer with no parent path" (checkModel commitReviewer == Just "gpt-6-luna")
  void $ turn (checkActor commitReviewer) "respond (Produced (Repair (reviewInput sessionInput) []))"
  void $ turn owner "finishWork reviewQuestions"
  void $ example (checkActor worker) "exomonad-review" 1
  reviewer <- activation
  void $ turn (checkActor reviewer) "let checks = [\"fixture review\"] :: [Text]\nlet scope = \"skill composition only\" :: Text"
  replied <- example (checkActor reviewer) "exomonad-review" 2
  check "text UX: successful reply explicitly reports submission" ("Reply submitted." `Text.isInfixOf` output replied)
  void $ turn (checkActor worker) "finishWork reviewQuestions"
  void $ turn (checkActor worker) "respond (Produced candidate)"
  awaitCell owner "collector retains terminal candidate and gates"
    ("do { observed <- readWork router; pure (case collectedWork observed of { [source] -> case sourceResult source of { Just (Right receipt) -> case responseValue receipt of { Produced candidate -> candidateCommit candidate == " <> gitOidLiteral baseline <> " && remainingGates candidate == [\"product acceptance remains\"]; _ -> False }; _ -> False }; _ -> False }) }")
  awaitCell owner "record skill receives the typed terminal source once"
    "(== 1) <$> R.call (resultCount (R.client results)) ()"
  void $ example owner "exomonad-define-actors" 2
  void $ example owner "exomonad-coordinate" 2
  assertCell owner "the coordinate skill retains its scoped cleanup receipt"
    "cleanupPlanGroup (cleanupReceiptPlan released) == cleanupPlanGroup (cleanupReceiptPlan cleanup) && not (null (cleanupReceiptSteps released))"

  -- The shipped decomposition example is a real multi-item cell. It uses an
  -- ordinary local selector, selected Task context and a selective collector.
  void $ turn owner ("let baseline = " <> gitOidLiteral baseline)
  void $ example owner "exomonad-coordinate" 3
  firstChild <- activation
  secondChild <- activation
  let children = sortOn checkLabel [firstChild, secondChild]
      [consumer, interface] = children
      labels = map checkLabel children
  check ("one plan yields two distinct Luna assignments with their shared contract: " <> Text.pack (show labels))
    (labels == ["corpus/fanout/consumer", "corpus/fanout/interface"]
      && all ((== Just "gpt-6-luna") . checkModel) children
      && all (Text.isInfixOf "The interface and consumer share one accepted contract" . checkContext) children)
  void $ example owner "exomonad-coordinate" 4
  void $ turn (checkActor interface) ("let found = Candidate " <> gitOidLiteral baseline <> " [\"interface evidence\"] []\nreportProgress (WorkProgress [found] [])")
  void $ turn (checkActor consumer) ("let found = Candidate " <> gitOidLiteral baseline <> " [\"consumer evidence\"] []\nreportProgress (WorkProgress [found] [])")
  awaitCell owner "collector retains both candidates without a routine wake"
    "do { observed <- readWork router; pure (length (collectedWork observed) == 2 && all (\\(name, evidence) -> any (\\source -> sourceName source == name && any (\\candidate -> reportedChecks candidate == [evidence]) (workEvidence (sourceProgress source))) (collectedWork observed)) [(\"interface\", \"interface evidence\"), (\"consumer\", \"consumer evidence\")]) }"
  void $ turn (checkActor interface) "respond (Produced found)"
  void $ turn (checkActor consumer) "respond (Produced found)"
  void $ turn owner "drained <- finishWorkBatch localBatch\nlet Just group = forkGroupHandle interface\nreleased <- releaseGroup group\ninspectFull released"

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
    "case answer of { Left _ -> True; Right a -> case J.takenUnder J.careful a of { Left doubt -> not (T.null doubt.why); Right (J.Settled verdict) -> (a.key, verdict) `elem` [(\"all_present\", \"merge: every item of the checklist holds\"), (\"one_absent\", \"repair: an item of the checklist does not hold\"), (\"contradicts\", \"escalate: the artifacts contradict each other\"), (\"insufficient_evidence\", \"ask again: name the missing field and re-ask\")] } }"
  void $ example owner "exomonad-jev" 3
  assertCell owner "text: the selected continuation runs and returns its command output"
    "case answer of { Left _ -> next == \"jev unavailable; inspecting by hand\"; Right _ -> next `elem` [\"would rerun session::retry_is_bounded\\n\", \"would rerun the suite\\n\", \"reading the failure by hand\", \"rerun unavailable\", \"suite unavailable\"] }"
  void $ example owner "exomonad-jev" 4

  -- The unfold skill's launch and watch cells are the published doc examples;
  -- these two read a child's identity and its submission without touching the
  -- child's own checkout.
  void $ example owner "exomonad-unfold" 2
  void $ example owner "exomonad-unfold" 3
  assertCell owner "a settled child is inspected through its typed submission evidence"
    ("case evidence of { Just (WorktreeObserved _ submitted _) -> submitted == " <> gitOidLiteral baseline <> "; _ -> False }")

  -- Cleanup is inspected before it is executed; the plan is a value.
  void $ example owner "exomonad-cleanup" 0
  assertCell owner "the cleanup skill retains its typed plan"
    "cleanupPlanGroup (cleanupReceiptPlan cleanupReceipt) == cleanupPlanGroup (cleanupReceiptPlan cleanup)"

  void $ example owner "exomonad-fork" 1
  parserChild <- activation
  storeChild <- activation
  testChild <- activation
  check "primitive fork example admits its three ready obligations"
    (all ((== Just "gpt-6-luna") . checkModel) [parserChild, storeChild, testChild])
  mapM_ (\child -> void $ turn (checkActor child) "respond (Blocked \"fixture complete\" [] :: Outcome Candidate)") [parserChild, storeChild, testChild]
  void $ turn owner "finishWork primitiveQuestions"
  checkNotebookForms owner

-- Run the pure authoring forms independently of the worker/review examples.
notebookForms :: Member RecipeCheck effects => Eff effects ()
notebookForms = root >>= checkNotebookForms

checkNotebookForms :: Member RecipeCheck effects => CheckActor -> Eff effects ()
checkNotebookForms owner = do
  -- The project-free record definition: no Exomonad.Contrib.Actors wrapper, the row
  -- named in the cell and pinned by a signature on the definition.
  void $ example owner "exomonad-define-actors" 3
  awaitCell owner "a record definition pins its own effect row without a project wrapper"
    "(== 1) <$> R.call (noteCount (R.client tally)) ()"

  -- Execute the authoring forms, then inspect their actual typed bindings.
  void $ example owner "exomonad-workbench" 8
  assertCell owner "text: a signature beside its equation installs both forms of binding"
    "map severity [1,3] == [\"low\",\"high\"] && inline 7 == \"work 7\""
  void $ example owner "exomonad-workbench" 9
  assertCell owner "an annotated literal settles an otherwise ambiguous encodable field"
    "state == object [\"owned_path\" .= (\"src/app.rs\" :: Text), \"changed\" .= (2 :: Int)]"
  void $ example owner "exomonad-workbench" 10
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
  void $ example owner "exomonad-review" 0
  reviewer <- activation
  check "review context carries the distinct cumulative base and candidate"
    (("Base: " <> baseline) `Text.isInfixOf` checkContext reviewer
      && ("Candidate: " <> candidate) `Text.isInfixOf` checkContext reviewer)
  actual <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "review checkout is the assigned candidate" (actual == candidate)
  void $ turn (checkActor reviewer) "let current = sessionInput :: ReviewRequest\nlet latest = reviewInput current"
  assertCell (checkActor reviewer) "typed request preserves both revision identities"
    ("reviewBase (reviewBasis current) == " <> gitOidLiteral baseline <> " && candidateCommit latest == " <> gitOidLiteral candidate)
  void $ turn (checkActor reviewer)
    "import qualified Tidepool.Agent.Reply as ReviewReply\noldScope <- (ReviewReply.currentRequest :: Eff CodingEffects (ReviewReply.RequestScope ReviewRequest (Outcome ReviewDecision)))\nlet oldId = case oldScope of { ReviewReply.RequestActive request _ -> ReviewReply.requestIdNumber request; _ -> -1 }"
  assertCell (checkActor reviewer) "request scope borrows the exact active review input"
    ("case oldScope of { ReviewReply.RequestActive _ active -> candidateCommit (reviewInput active) == " <> gitOidLiteral candidate <> "; _ -> False }")
  void $ turn (checkActor reviewer)
    "wrongInput <- (ReviewReply.currentRequest :: Eff CodingEffects (ReviewReply.RequestScope () (Outcome ReviewDecision)))\nwrongResult <- (ReviewReply.currentRequest :: Eff CodingEffects (ReviewReply.RequestScope ReviewRequest ()))"
  assertCell (checkActor reviewer) "wrong request input and result types refuse before borrowing"
    "case (wrongInput, wrongResult) of { (ReviewReply.RequestUnavailable ReviewReply.RequestTypeMismatch, ReviewReply.RequestUnavailable ReviewReply.RequestTypeMismatch) -> True; _ -> False }"
  void $ turn (checkActor reviewer) "let checks = [\"recipe verified candidate checkout\"] :: [Text]\nlet conclusion = \"review provenance fixture\" :: Text"
  prompt <- readFile (checkActor reviewer) (Text.pack workspaceRoot <> "/prompts/review.md")
  let section = snd (Text.breakOn "For acceptance" prompt)
      blocks = map (fst . Text.breakOn "```") (drop 1 (Text.splitOn "```haskell\n" section))
  void $ turn (checkActor reviewer) (head blocks)
  void $ turn owner "accepted <- waitFor (awaitResponse reviewer)"
  assertCell owner "exact acceptance retains its scope without fabricating a Task"
    "case accepted of { Right receipt -> case responseValue receipt of { Produced (Accepted checked) -> case reviewedBasis checked of { ExactScope originalBase _ _ -> originalBase == base && candidateCommit (reviewedCandidate checked) == commit; _ -> False }; _ -> False }; _ -> False }"
  -- Exact review retries retain the real scope, including when the caller offers
  -- a retained implementer: there is no implementation Task to assign to it.
  void $ turn owner "let exact = ReviewRequest (ExactScope base [\"review-provenance.txt\"] \"exact acceptance\") (Candidate commit [] [\"remaining gate\"]) (RetainedImplementer (responseActor reviewer))\n(retry, retryProgress) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewer) (assignment [label|exact-retry|] exact)\nretryRetention <- detachRequest retry"
  assertCell owner "exact review retry retains its request"
    "case retryRetention of { Right () -> True; _ -> False }"
  void $ turn (checkActor reviewer) "let current = sessionInput :: ReviewRequest\nlet latest = reviewInput current\nverdict <- repair [label|exact-findings|] current latest [\"repair at the owner\"]"
  assertCell (checkActor reviewer) "exact-scope repair returns findings without inventing an implementation assignment"
    "case verdict of { Left (Repair found issues) -> found == latest && issues == [\"repair at the owner\"]; _ -> False }"
  void $ turn (checkActor reviewer) "respond (Produced (Repair latest [\"repair at the owner\"]))"
  revised <- checkpoint owner "review-provenance.txt" "revised review candidate\n" "review repair fixture"
  void $ git (checkActor reviewer) ["merge", "--ff-only", revised]
  void $ turn owner ("let revisedCommit = " <> gitOidLiteral revised)
  void $ turn owner "let question = Question \"contract\" (DesignQuestion \"plan.md\" base \"boundary\" [\"evidence\"] [] [])\nlet decision = AcceptedDecision question base \"preserve the boundary\" [\"checked\"]\nlet assigned = (task [label|assigned-review|] \"review implementation\" [\"review-provenance.txt\"] \"assigned acceptance\" base) { planPath = \"plan.md\", rationale = \"retained reason\", acceptedDecisions = [decision] }\nlet assignedRequest = ReviewRequest (AssignedTask assigned) (Candidate revisedCommit [\"candidate check\"] [\"open gate\"]) OwnerRepairs\n(assignedReview, assignedProgress) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewer) (assignment [label|assigned-retry|] assignedRequest)\nassignedReviewRetention <- detachRequest assignedReview"
  assertCell owner "assigned review retry retains its request"
    "case assignedReviewRetention of { Right () -> True; _ -> False }"
  void $ turn (checkActor reviewer) "let current = sessionInput :: ReviewRequest\nlet latest = reviewInput current"
  void $ turn (checkActor reviewer)
    "newScope <- (ReviewReply.currentRequest :: Eff CodingEffects (ReviewReply.RequestScope ReviewRequest (Outcome ReviewDecision)))\nlet newId = case newScope of { ReviewReply.RequestActive request _ -> ReviewReply.requestIdNumber request; _ -> -1 }"
  assertCell (checkActor reviewer) "a prepared retained request replaces identity and mounted input"
    ("case newScope of { ReviewReply.RequestActive request active -> ReviewReply.requestIdNumber request /= oldId && candidateCommit (reviewInput active) == " <> gitOidLiteral revised <> "; _ -> False }")
  void $ turn (checkActor reviewer)
    ("import qualified Project.ReviewTools as ReviewTool\nimport Tidepool.Agent.Contract (handler)\nlet submit = handler (ReviewTool.submit_review ReviewTool.tools)\nstale <- submit (ReviewTool.ReviewSubmitInput oldId "
      <> gitOidLiteral revised <> " [\"checked revised candidate\"] \"assigned acceptance\")")
  assertCell (checkActor reviewer) "review tool refuses the previous request id"
    "case stale of { ReviewTool.RequestIdMismatch _ -> True; _ -> False }"
  void $ turn (checkActor reviewer)
    ("wrong <- submit (ReviewTool.ReviewSubmitInput newId " <> gitOidLiteral candidate
      <> " [\"checked revised candidate\"] \"assigned acceptance\")")
  assertCell (checkActor reviewer) "review tool refuses the previous candidate commit"
    "case wrong of { ReviewTool.CandidateOidMismatch _ -> True; _ -> False }"
  writeFile (checkActor reviewer) "review-provenance.txt" "dirty review checkout\n"
  void $ turn (checkActor reviewer)
    ("dirty <- submit (ReviewTool.ReviewSubmitInput newId " <> gitOidLiteral revised
      <> " [\"checked revised candidate\"] \"assigned acceptance\")")
  assertCell (checkActor reviewer) "review tool refuses a dirty checkout"
    "case dirty of { ReviewTool.ReviewCheckoutDirty -> True; _ -> False }"
  void $ git (checkActor reviewer) ["restore", "--", "review-provenance.txt"]
  void $ turn (checkActor reviewer)
    ("submit (ReviewTool.ReviewSubmitInput newId " <> gitOidLiteral revised
      <> " [\"checked revised candidate\"] \"assigned acceptance\")")
  void $ turn owner "retained <- waitFor (awaitResponse assignedReview)"
  assertCell owner "assigned acceptance preserves the whole Task, decisions and candidate gates"
    "case retained of { Right receipt -> case responseValue receipt of { Produced (Accepted checked) -> reviewedBasis checked == AssignedTask assigned && reviewedCandidate checked == reviewInput assignedRequest; _ -> False }; _ -> False }"
