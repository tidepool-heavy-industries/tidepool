{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- Judging one candidate. `Review` is the harness's entry point: one record
-- actor per task instance, holding every Jev seam -- mechanical evidence
-- intake, reflex classification, the acceptance gate, admitting a read-only
-- reviewer, verdict routing, repeat-defect detection, risk scoring, and
-- stuck detection. Read by whoever starts a review per implementer; it is
-- the only caller of Project.Merge's `publish`.
--
-- One review actor per task instance. Shape, in five lines:
--   1. It subscribes to one implementer's settlement and progress, and owns
--      that task from first candidate to merged.
--   2. Mechanical evidence is derived here, in the parent's custody: the OID
--      off `responseWorktree`, then `git diff` for stat and hunks, then
--      coverage, ownership, required-test and `ok`-line checks in code.
--   3. Every seam that is not a string match is a Jev call: eight of them,
--      each with an `insufficient_evidence` option, each written to the
--      history.
--   4. Review is never skipped. Every candidate that passes the mechanical
--      checks and the Jev acceptance gate still gets a fresh read-only Luna
--      reviewer; Jev routes the verdict and composes the brief, it never
--      replaces the reviewer. Run 6's Task 4 accepted a false candidate at
--      0.90 confidence.
--   5. The root is notified only on the seven alert conditions. A normal
--      outcome is a history row, not a message; the root reads every review
--      in one cell with
--        traverse (\r -> R.call (reviewView (R.client r)) ()) reviews
--      and `Show ReviewState` is one line per history entry so that cell
--      fits.
--
-- "merged" lines are informational: the review sends exactly one when its
-- task is finished (merged and the integrated check green) so the root
-- learns completion, and that line asks for nothing. Nothing else about a
-- normal candidate reaches the root as a notification.
--
-- This is the project's own code, written over the shipped surface. Nothing
-- here belongs in a library: another project's loop wants different seams.
module Project.Review
  ( -- The actor
    Review (..)
  , ReviewEffects
  , ReviewerEffects
  , ReviewState (..)
  , reviewOf
  , reviewActor
    -- Pure helpers the root may reuse from a cell
  , riskCount
  , riskCountAt
  , mergeOrder
  ) where

import Control.Monad (void)
import Data.Maybe (fromMaybe)
import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)

import qualified Jev.Operators as J
import Jev.Operators (Packet ((:=), (:&)), Settled (Settled))
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import qualified Tidepool.Command as Cmd
import Tidepool.Actors.Exomonad hiding (record)
import Tidepool.Aeson.Value (object, (.=))
import Tidepool.Effects.Core (GitRef (..), Jev, Commands)
import Tidepool.Worktree (renderGitOid)

import Project.Contract
import Project.Evidence
import qualified Project.Merge as Merge
import Project.Merge (MergeTarget (..), MergeResult (..), publish, exitCode)
import Project.Reflex (classify, jevClasses)

-- The root sorts finished reviews in code from the Noul battery of seam (f):
-- lowest risk merges first, ties broken by task name.
riskCount :: ReviewState -> Int
riskCount state =
  riskCountAt (noulFloor (contractPolicy (reviewContract state))) (reviewRisk state)

-- | Count only risks above the contract's Noul floor. Kept pure so the
-- deterministic ordering rule can be checked without asking Jev to score a
-- candidate.
riskCountAt :: Double -> [(Text, Double)] -> Int
riskCountAt floor' risks = length [key | (key, score) <- risks, score > floor']

mergeOrder :: [ReviewState] -> [(Text, Int)]
mergeOrder states = foldr insert [] [(contractTask (reviewContract s), riskCount s) | s <- states]
  where
    insert entry [] = [entry]
    insert entry (head' : rest)
      | snd entry < snd head' = entry : head' : rest
      | snd entry == snd head' && fst entry <= fst head' = entry : head' : rest
      | otherwise = head' : insert entry rest

-- ---------------------------------------------------------------------------
-- The actor
-- ---------------------------------------------------------------------------

-- The review's row: `unfold` needs AgentInspection and Forks, admitting a
-- read-only reviewer needs the reviewer's row to be a subset of this one
-- (Watches, ActorContext, BoundWorktree), Jev and Commands for the seams and
-- the evidence, Actor to call the merge actor. No WorktreeIntegration: the
-- review holds no worktree, so it sits under the research ceiling.
type ReviewEffects = R.LocalEffects Review
  '[ Replies, Watches, Forks, ActorContext
   , AgentInspection, BoundWorktree
   , Notifications, Jev, Commands, Actor
   ]

-- A child's row must be a subset of this actor's, so the reviewer's row is
-- written out and narrowed explicitly rather than inherited: no Forks (it
-- cannot spawn), no AgentInspection, no WorktreeIntegration (it cannot
-- merge). `inspectionPolicy` puts it in the inspect-only role at the
-- candidate's ref. The shipped `researchingLeaf` is the same row under a
-- shorter name.
type ReviewerEffects =
  '[ Replies, Watches, ActorContext, BoundWorktree
   , Notifications, Jev, Commands, Actor
   ]

data Review mode = Review
  { reviewStateField :: mode :- State ReviewState
  , reviewView :: mode :- Call () (R.Reply ReviewState)
  , candidateSettled :: mode :- Event (Either ResponseFailure (ResponseResult ImplReport))
  , workerProgress :: mode :- Event (ProgressState ImplNote)
  , repairSettled :: mode :- Call (Either ResponseFailure (ResponseResult ImplReport)) NoReply
  , reviewSettled :: mode :- Call (Either ResponseFailure (ResponseResult ReviewVerdict)) NoReply
  } deriving Generic

data ReviewState = ReviewState
  { reviewContract :: Contract
  , reviewMerge :: MergeTarget
  , reviewWorker :: Response ImplReport
  , reviewEvidence :: [Evidence]
  , reviewFindings :: [[Text]]
  , reviewRepairs :: Int
  , reviewRisk :: [(Text, Double)]
  , reviewStalls :: Int
  , reviewLastNote :: Maybe ImplNote
  , reviewMerged :: Maybe GitOid
  , reviewCheck :: Maybe CheckResult
  , reviewHistory :: [HistoryEntry]
  , reviewNotices :: [ReviewNotice]
  , reviewReceipts :: [Either NotificationError NotificationReceipt]
  }

-- One line per history entry, headed by one line of position. This is what
-- the root reads for every review at once, so it never grows a second
-- dimension.
instance Show ReviewState where
  show state = unlines $
    ( Text.unpack (contractTask (reviewContract state))
      ++ " candidates=" ++ show (length (reviewEvidence state))
      ++ " repairs=" ++ show (reviewRepairs state)
      ++ " merged=" ++ Text.unpack (maybe "-" shortOid (reviewMerged state))
      ++ " check=" ++ maybe "-" show (reviewCheck state)
      ++ " notices=" ++ show (length (reviewNotices state))
    ) : map show (reviewHistory state)

reviewActor
  :: Text
  -> Review (Definition (Handler ReviewState ReviewEffects))
  -> ActorSpec Review ReviewEffects
reviewActor name = R.definition name (Actor.Selected knownEffects)

-- | The parent starts one merge actor per merge target, then one review per
-- implementer right after admission:
--
-- > Right tree <- createWorktree (fromRef "exomonad/integration" "integration")
-- > merge <- R.start (mergeInto (worktreeId tree) (Just "exomonad/integration")
-- >                     ["just", "test-lib", "exomonad-actor", "test(request::updates)"])
-- > worker <- unfold group (childWithProgress @ImplNote @ImplReport branch)
-- > review <- R.start (reviewOf contract worker (MergeTarget merge))
reviewOf
  :: Contract
  -> (Response ImplReport, Progress ImplNote)
  -> MergeTarget
  -> ActorSpec Review ReviewEffects
reviewOf contract (worker, updates) target =
  reviewActor ("review-" <> contractTask contract) Review
    { reviewStateField = ReviewState
        { reviewContract = contract
        , reviewMerge = target
        , reviewWorker = worker
        , reviewEvidence = []
        , reviewFindings = []
        , reviewRepairs = 0
        , reviewRisk = []
        , reviewStalls = 0
        , reviewLastNote = Nothing
        , reviewMerged = Nothing
        , reviewCheck = Nothing
        , reviewHistory = []
        , reviewNotices = []
        , reviewReceipts = []
        }
    , reviewView = \() -> R.get
    , candidateSettled = R.on (R.settlement worker) $ \result -> do
        own <- R.self @Review
        onCandidate own result
    , workerProgress = R.on (R.progress updates) onProgress
    , repairSettled = \result -> do
        own <- R.self @Review
        onCandidate own result
    , reviewSettled = onReview
    }

-- ---------------------------------------------------------------------------
-- History and notice plumbing
-- ---------------------------------------------------------------------------

currentPolicy :: Handler ReviewState ReviewEffects ReviewPolicy
currentPolicy = R.gets (contractPolicy . reviewContract)

record
  :: Text -> Maybe GitOid -> CheckSource -> Text -> Text -> Text
  -> Handler ReviewState ReviewEffects Int
record seam candidate source key detail action = do
  state <- R.get
  let index = length (reviewHistory state)
  R.put state
    { reviewHistory = reviewHistory state
        ++ [HistoryEntry index seam candidate source key detail action]
    }
  pure index

-- The only path from the review to the root's attention.
notify'
  :: NoticeKind -> Maybe GitOid -> Text -> CheckSource -> Int -> Text
  -> Handler ReviewState ReviewEffects ()
notify' kind candidate condition source index suggested = do
  state <- R.get
  let notice = ReviewNotice
        { noticeKind = kind
        , noticeTask = contractTask (reviewContract state)
        , noticeCandidate = candidate
        , noticeChecksPassed = passedChecks state
        , noticeCondition = condition
        , noticeSource = source
        , noticeEntry = index
        , noticeSuggested = suggested
        }
  sent <- sendMessage (contractOwner (reviewContract state)) (renderNotice notice)
  R.modify' (\current -> current
    { reviewNotices = reviewNotices current ++ [notice]
    , reviewReceipts = reviewReceipts current ++ [sent]
    })

passedChecks :: ReviewState -> [Text]
passedChecks state =
  [ checkName result
  | evidence <- take 1 (reverse (reviewEvidence state))
  , result <- evidenceChecks evidence
  , checkPassed result
  ] ++ [ checkName result | Just result <- [reviewCheck state], checkPassed result ]

-- Every Jev seam funnels its failure here: a `Left` from the host, a `Doubt`
-- from the policy, and the literal `insufficient_evidence` key are the same
-- event to the root, and each of them stops the review rather than guessing.
doubted
  :: Text -> Maybe GitOid -> Text -> Text
  -> Handler ReviewState ReviewEffects ()
doubted seam candidate detail suggested = do
  index <- record seam candidate RanHere "insufficient_evidence" detail suggested
  notify' Alert candidate (seam <> " gave no usable answer: " <> detail)
    RanHere index suggested

-- ---------------------------------------------------------------------------
-- Evidence in the review's own custody
-- ---------------------------------------------------------------------------

-- A git command that did not exit 0 is a failure of the evidence, never an
-- empty comparison: `coverageCheck "" ""` would pass.
gitText :: [Text] -> Handler ReviewState ReviewEffects (Either Text Text)
gitText arguments = do
  result <- Cmd.run (Cmd.argv ("git" : arguments))
  let code = exitCode result
  pure $ case (code, Cmd.stdout result) of
    (0, Right out) -> Right out
    (0, Left issue) -> Left ("git " <> Text.unwords arguments <> ": unreadable stdout: " <> Text.pack (show issue))
    (_, _) -> Left ("git " <> Text.unwords arguments <> " "
                     <> fromMaybe ("exited " <> Text.pack (show code)) (Cmd.failure result))

candidateOid :: WorktreeEvidence -> Maybe GitOid
candidateOid evidence = case evidence of
  WorktreeObserved _ _ observation -> Just (headOid (submittedHead observation))
  _ -> Nothing

deriveEvidence
  :: Contract -> GitOid -> ImplReport
  -> Handler ReviewState ReviewEffects (Either Text Evidence)
deriveEvidence contract oid report = do
  let range = renderGitOid (contractBase contract) <> ".." <> renderGitOid oid
  statOrFailure <- gitText ["diff", "--numstat", range]
  hunksOrFailure <- gitText (["diff", "--unified=80", range, "--"] ++ contractOwnedPaths contract)
  case (statOrFailure, hunksOrFailure) of
    (Left failure, _) -> pure (Left failure)
    (_, Left failure) -> pure (Left failure)
    (Right stat, Right hunks) -> pure (Right (assemble stat hunks))
  where
   assemble stat hunks =
    let output = reportOutput report
        absent = missingTests (contractRequiredTests contract) hunks output
        tests = CheckResult "required-tests" RanHere (null absent)
          (if null absent then "all present and reported ok"
           else "missing " <> Text.intercalate ", " absent)
        claims = CheckResult "claimed-command" ChildReported
          (not (Text.null (reportCommand report))) (reportCommand report)
    in Evidence
      { evidenceCandidate = oid
      , evidenceStat = stat
      , evidenceHunks = hunks
      , evidenceOutput = output
      , evidenceChecks =
          [ coverageCheck stat hunks
          , ownershipCheck (contractOwnedPaths contract) stat
          , tests
          , claims
          ]
        -- The child's literal output is classified by the table before any Jev
        -- call. `reflexFor`'s exit-0 rule belongs to a command this review ran
        -- itself, so the child's text goes through `classify`.
      , evidenceReflex = classify output
      }

-- ---------------------------------------------------------------------------
-- The candidate handler: mechanical first, reflex second, Jev after that
-- ---------------------------------------------------------------------------

onCandidate
  :: Review Self
  -> Either ResponseFailure (ResponseResult ImplReport)
  -> Handler ReviewState ReviewEffects ()
onCandidate own result = do
  contract <- R.gets reviewContract
  case result of
    Left failure -> do
      index <- record "mechanical" Nothing RanHere "response_failed"
        (Text.pack (show failure)) "root decides whether to re-admit"
      notify' Alert Nothing ("the implementer did not settle: " <> Text.pack (show failure))
        RanHere index "re-admit the child or drop the task"
    Right receipt -> case candidateOid (responseWorktree receipt) of
      Nothing -> do
        index <- record "mechanical" Nothing RanHere "no_submission"
          "the reply carries no bound-worktree evidence" "root decides"
        notify' Alert Nothing "the reply carries no submitted OID"
          RanHere index "ask the child to commit, or re-admit it"
      Just oid -> deriveEvidence contract oid (responseValue receipt) >>= \derived -> case derived of
       Left failure -> do
        index <- record "mechanical" (Just oid) RanHere "evidence_unavailable" failure
          "root decides; the candidate was not compared"
        notify' Alert (Just oid) ("could not obtain evidence: " <> failure) RanHere index
          "check the candidate OID and the base, then re-derive or drop"
       Right evidence -> do
        R.modify' (\state -> state { reviewEvidence = reviewEvidence state ++ [evidence] })
        let failures = [check | check <- evidenceChecks evidence
                              , not (checkPassed check), checkSource check == RanHere]
        residue <- classifyResidue evidence
        case failures of
          failed : _ -> do
            index <- record "mechanical" (Just oid) RanHere (checkName failed)
              (checkDetail failed <> reflexSuffix residue)
              "request repair naming the failed check"
            notify' Alert (Just oid)
              (checkName failed <> " failed: " <> checkDetail failed)
              RanHere index "the review is requesting the repair; confirm or take it over"
            requestRepair own oid [checkName failed <> ": " <> checkDetail failed]
          [] -> do
            honest <- honestyTieBreaker contract evidence
            if honest then runAcceptance own contract evidence else pure ()

reflexSuffix :: Maybe Text -> Text
reflexSuffix Nothing = ""
reflexSuffix (Just klass) = " (reflex " <> klass <> ")"

-- ---------------------------------------------------------------------------
-- The reflex seam: reflex residue. The table decides first; only output that matched
-- nothing at all is handed to Jev, over the table's own eleven criteria.
-- Policy: routing.
-- ---------------------------------------------------------------------------

classifyResidue :: Evidence -> Handler ReviewState ReviewEffects (Maybe Text)
classifyResidue evidence = case evidenceReflex evidence of
  Just matched -> do
    void (record "reflex" (Just (evidenceCandidate evidence)) RanHere
      (Text.pack (show matched)) "matched the table in code" "no Jev call")
    pure (Just (Text.pack (show matched)))
  Nothing
    | Text.null (Text.strip (evidenceOutput evidence)) -> do
        doubted "reflex" (Just (evidenceCandidate evidence))
          "the child reported no literal output to classify"
          "ask the child for the literal command output"
        pure Nothing
    | otherwise -> do
        policy <- currentPolicy
        let classNames = map fst jevClasses
            options = J.many #klass id (\k -> maybe "" id (lookup k jevClasses)) classNames
              J..| J.alt #insufficient_evidence
                    "`check_output` names no compiler code, lint name, test marker or environment marker, and no sentence in it describes a failure" ("insufficient_evidence" :: Text)
        answer <- J.ask1
          (J.rawState (object
            [ "check_output" .= Text.takeEnd 4000 (evidenceOutput evidence)
            , "matched_table" .= ("no entry in the reflex table matched" :: Text)
            ]))
          (J.choice "Which criteria describe `check_output`?" options)
        case answer of
          Left failure -> do
            doubted "reflex" (Just (evidenceCandidate evidence))
              ("jev unavailable: " <> Text.pack (show failure))
              "read the output yourself"
            pure Nothing
          Right chosen -> case J.takenUnder (policyReflex policy) chosen of
            Left doubt -> do
              doubted "reflex" (Just (evidenceCandidate evidence))
                (Text.pack (show doubt) <> "; " <> J.explain (policyReflex policy) chosen)
                "read the output yourself"
              pure Nothing
            Right (Settled key)
              | key == "insufficient_evidence" -> do
                  doubted "reflex" (Just (evidenceCandidate evidence))
                    "no criterion applies to this output" "read the output yourself"
                  pure Nothing
              | otherwise -> do
                  void (record "reflex" (Just (evidenceCandidate evidence)) RanHere
                    key (J.explain (policyReflex policy) chosen)
                    "carried into the review state")
                  pure (Just key)

-- ---------------------------------------------------------------------------
-- The honesty seam: honesty tie-breaker. One Noul per claimed test over the child's
-- literal output, plus one exit Noul. No policy: Nouls are thresholds.
-- ---------------------------------------------------------------------------

honestyTieBreaker :: Contract -> Evidence -> Handler ReviewState ReviewEffects Bool
honestyTieBreaker contract evidence
  | null (contractRequiredTests contract) = pure True
  | Text.null (Text.strip (evidenceOutput evidence)) = do
      doubted "honesty" (Just (evidenceCandidate evidence))
        "no literal output to compare the claims against"
        "ask the child for the literal test output"
      pure False
  | otherwise = do
      let packet =
            #legible := J.noul "Does `test_output` contain lines in the form `test <name> ... ok` or `<name> ... FAILED`?"
              :& #claimed := J.each id
                   (\name -> #reported := J.noul ("The test named " <> name
                        <> ". Does `test_output` contain a line reporting this test as passing?"))
                   (contractRequiredTests contract)
      answer <- J.ask
        (J.rawState (object
          [ "test_output" .= Text.takeEnd (outputBudget (contractPolicy contract)) (evidenceOutput evidence)
          , "test_output_truncated" .= (Text.length (evidenceOutput evidence) > outputBudget (contractPolicy contract))
          , "test_output_bytes" .= Text.length (evidenceOutput evidence)
          , "files_changed" .= [path | (_, _, path) <- numstatFiles (evidenceStat evidence)]
          ]))
        packet
      case answer of
        Left failure -> do
          doubted "honesty" (Just (evidenceCandidate evidence))
            ("jev unavailable: " <> Text.pack (show failure))
            "compare the claims and the output yourself"
          pure False
        Right response -> do
          let a = J.answers response
          if a.legible.yes < (noulFloor (contractPolicy contract))
            then do
              doubted "honesty" (Just (evidenceCandidate evidence))
                "`test_output` does not carry test result lines at all"
                "ask the child for the literal test output"
              pure False
            else do
              let unsupported = [name | (name, per) <- a.claimed, per.reported.yes < (noulFloor (contractPolicy contract))]
              void (record "honesty" (Just (evidenceCandidate evidence)) ChildReported
                (if null unsupported then "claims_supported" else "claims_unsupported")
                (Text.intercalate ", " unsupported)
                (if null unsupported then "continue to the review" else "notify the root"))
              if null unsupported
                then pure True
                else do
                  index <- record "honesty" (Just (evidenceCandidate evidence)) ChildReported
                    "claims_unsupported"
                    ("not reported passing: " <> Text.intercalate ", " unsupported)
                    "request repair"
                  notify' Alert (Just (evidenceCandidate evidence))
                    ("claimed but not in the output: " <> Text.intercalate ", " unsupported)
                    ChildReported index "request repair"
                  pure False

-- ---------------------------------------------------------------------------
-- The acceptance seam: the acceptance gate. Policy: merging. The likely-miss condition
-- the root wrote in its pre-wave pass is enumerated verbatim in
-- `item_missing`.
-- ---------------------------------------------------------------------------

runAcceptance
  :: Review Self -> Contract -> Evidence
  -> Handler ReviewState ReviewEffects ()
runAcceptance own contract evidence = do
  let oid = evidenceCandidate evidence
      policy = contractPolicy contract
      keyed = [ ("item_" <> Text.pack (show n), item)
              | (n, item) <- zip [1 :: Int ..] (contractChecklist contract) ]
      items = Text.intercalate "; " (contractChecklist contract)
      -- The packet is bounded first, then validated: Jev is never asked to
      -- discover truncation from truncated text.
      (packetHunks, omitted) = fitHunks (hunksBudget policy) (evidenceHunks evidence)
      outputTruncated = Text.length (evidenceOutput evidence) > outputBudget policy
      packetOutput = Text.takeEnd (outputBudget policy) (evidenceOutput evidence)
      packetCoverage = coverageCheck (evidenceStat evidence) packetHunks
  if not (null omitted)
    then doubted "acceptance" (Just oid)
      ("the hunks exceed one packet's budget; files omitted: " <> Text.intercalate ", " omitted)
      "narrow the contract's owned paths or review this candidate by hand"
    else if not (checkPassed packetCoverage)
    then doubted "acceptance" (Just oid)
      ("the packet does not cover the stat: " <> checkDetail packetCoverage)
      "re-derive the evidence before asking again"
    else do
      -- One Noul per checklist item: narrow questions find what one broad
      -- choice confidently misses. The choice stays as the summary.
      let packet =
            -- A canary with a known answer rides in every packet: a miss says
            -- the packet is broken, not the candidate (measured E37B).
            #canary := J.noul "Does `diff_stat` name at least one file?"
              :& #covered := J.noul "Does `hunks` contain a hunk for every file named in `diff_stat`?"
              :& #likely_miss := J.noul ("Reading `hunks` and `test_output`, does the candidate handle this condition: " <> contractLikelyMiss contract)
              :& #each := J.each fst
                   (\(_, item) -> #holds := J.noul (item
                        <> "\n\nReading `hunks` and `test_output`, does the candidate satisfy this checklist item?"))
                   keyed
              :& #gate := J.choice "Which statement describes the candidate?"
                   ( J.alt #all_present
                       ("Every item of the checklist holds: " <> items <> ".")
                       ("all_present" :: Text)
                     J..| J.alt #item_missing
                       ("At least one item does not hold: a changed file outside the owned paths, a failing or missing owned test, a deleted or weakened test, a remaining todo!(), an implementation that does not match the goal, or this condition: " <> contractLikelyMiss contract)
                       "item_missing"
                     J..| J.alt #conflicting
                       "The items are all present but contradict each other, for example the report claims a test passes that `test_output` shows failing."
                       "conflicting"
                     J..| J.alt #insufficient_evidence
                       "`hunks` or `test_output` is empty or truncated, so the items cannot be read off the state at all."
                       "insufficient_evidence" )
      answer <- J.ask
        (J.rawState (object
          [ "owned_paths" .= contractOwnedPaths contract
          , "acceptance_checklist" .= contractChecklist contract
          , "base" .= renderGitOid (contractBase contract)
          , "candidate" .= renderGitOid oid
          , "diff_stat" .= evidenceStat evidence
          , "hunks" .= packetHunks
          , "hunks_complete" .= True
          , "test_output" .= packetOutput
          , "test_output_truncated" .= outputTruncated
          , "test_output_bytes" .= Text.length (evidenceOutput evidence)
          ]))
        packet
      case answer of
        Left failure -> doubted "acceptance" (Just oid)
          ("jev unavailable: " <> Text.pack (show failure)) "read the hunks yourself"
        Right response -> do
          let a = J.answers response
              acceptPolicy = policyAccept policy
              -- Three outcomes per item: supported satisfaction, supported
              -- violation, unresolved. Only a supported violation becomes a
              -- repair instruction; unresolved items are a doubt.
              verdicts = [ (item, per.holds.yes) | ((_, item), per) <- a.each ]
              violated = [ item | (item, yes) <- verdicts, yes <= itemViolated policy ]
                ++ [ "likely miss: " <> contractLikelyMiss contract | a.likely_miss.yes <= itemViolated policy ]
              unresolved = [ item | (item, yes) <- verdicts, yes > itemViolated policy, yes < itemSatisfied policy ]
                ++ [ "likely miss: " <> contractLikelyMiss contract
                   | a.likely_miss.yes > itemViolated policy, a.likely_miss.yes < itemSatisfied policy ]
              itemDetail = Text.intercalate " | "
                [ item <> "=" <> Text.pack (show yes) | (item, yes) <- verdicts ]
                <> " | likely_miss=" <> Text.pack (show a.likely_miss.yes)
          void (record "acceptance" (Just oid) RanHere "items" itemDetail "per-item evidence")
          if a.canary.yes < noulFloor policy
            then doubted "acceptance" (Just oid)
              ("the packet canary missed: diff_stat names files, Jev said " <> Text.pack (show a.canary.yes))
              "the packet is broken, not the candidate; read the history row and the state size"
            else if a.covered.yes < noulFloor policy
            then doubted "acceptance" (Just oid)
              "the tripwire says a file in `diff_stat` has no hunk"
              "re-derive the diff before asking again"
            else case J.settle acceptPolicy a.gate
                ( #all_present (\_ ->
                    if not (null violated) then do
                      void (record "acceptance" (Just oid) RanHere "item_violated"
                        (Text.intercalate "; " violated) "request repair naming the items")
                      requestRepair own oid violated
                    else if not (null unresolved) then
                      doubted "acceptance" (Just oid)
                        ("items unresolved by the evidence: " <> Text.intercalate "; " unresolved)
                        "read the hunks for these items, then accept or request repair"
                    else do
                      void (record "acceptance" (Just oid) RanHere "all_present"
                        (J.explain acceptPolicy a.gate) "admit a reviewer")
                      admitReviewer own contract evidence)
                J..| #item_missing (\_ -> do
                    index <- record "acceptance" (Just oid) RanHere "item_missing"
                      (J.explain acceptPolicy a.gate) "request repair"
                    notify' Alert (Just oid) "the review says item_missing" RanHere index
                      "the review is requesting the repair; confirm or take it over"
                    requestRepair own oid
                      (if null violated then ["item_missing against the checklist: " <> items] else violated))
                J..| #conflicting (\_ -> do
                    index <- record "acceptance" (Just oid) RanHere "conflicting"
                      (J.explain acceptPolicy a.gate) "notify the root"
                    notify' Alert (Just oid)
                      "the report's claims contradict the evidence" RanHere index
                      "compare the report with `test_output`; repair or drop the candidate")
                J..| #insufficient_evidence (\_ -> doubted "acceptance" (Just oid)
                    "the review could not read the items off the state"
                    "re-derive the evidence, then decide by hand") ) of
              Left doubt -> do
                index <- record "acceptance" (Just oid) RanHere "doubt"
                  (Text.pack (show doubt) <> "; " <> J.explain acceptPolicy a.gate) "notify the root"
                notify' Alert (Just oid)
                  ("the review doubted the candidate: " <> J.explain acceptPolicy a.gate)
                  RanHere index "read the hunks, then accept or request repair"
              Right (Settled action) -> action

-- ---------------------------------------------------------------------------
-- The brief seam: reviewer brief composition. Which evidence fields the reviewer
-- needs. Accepted with the spawning policy because it gates the admission.
-- Review itself is never skipped.
-- ---------------------------------------------------------------------------

admitReviewer
  :: Review Self -> Contract -> Evidence
  -> Handler ReviewState ReviewEffects ()
admitReviewer own contract evidence = do
  let oid = evidenceCandidate evidence
  answer <- J.ask1
    (J.rawState (object
      [ "owned_paths" .= contractOwnedPaths contract
      , "diff_stat" .= evidenceStat evidence
      , "hunk_bytes" .= Text.length (evidenceHunks evidence)
      , "test_output_bytes" .= Text.length (evidenceOutput evidence)
      , "acceptance_checklist" .= contractChecklist contract
      ]))
    (J.choice "Which statement describes what a reviewer needs to judge this candidate?"
      ( J.alt #hunks_only
          "`diff_stat` names only files whose changes are self-contained, and no checklist item mentions a test." ("hunks_only" :: Text)
        J..| J.alt #hunks_and_tests
          "At least one checklist item names a test, so the hunks have to be read against `test_output`." "hunks_and_tests"
        J..| J.alt #full_check_output
          "`diff_stat` names a file outside the checklist's subject, or a checklist item is about a whole-repository check." "full_check_output"
        J..| J.alt #insufficient_evidence
          "`diff_stat` is empty, or `hunk_bytes` is zero, so what the reviewer would read is not established." "insufficient_evidence" ))
  let compose key = ReviewBrief
        { briefTask = contractTask contract
        , briefOwnedPaths = contractOwnedPaths contract
        , briefChecklist = contractChecklist contract
        , briefLikelyMiss = contractLikelyMiss contract
        , briefBase = renderGitOid (contractBase contract)
        , briefCandidate = renderGitOid oid
        , briefHunks = evidenceHunks evidence
        , briefTestOutput = if key == "hunks_only" then Nothing else Just (evidenceOutput evidence)
        , briefCheckOutput = if key == "full_check_output" then Just (evidenceStat evidence) else Nothing
        }
  case answer of
    Left failure -> do
      void (record "brief" (Just oid) RanHere "jev_unavailable"
        (Text.pack (show failure)) "admit the reviewer with hunks and tests")
      startReviewer own contract (compose "hunks_and_tests") oid
    Right chosen -> case J.takenUnder (policyBrief (contractPolicy contract)) chosen of
      Left doubt -> do
        void (record "brief" (Just oid) RanHere "doubt"
          (Text.pack (show doubt) <> "; " <> J.explain (policyBrief (contractPolicy contract)) chosen)
          "admit the reviewer with hunks and tests")
        startReviewer own contract (compose "hunks_and_tests") oid
      Right (Settled key)
        | key == "insufficient_evidence" ->
            doubted "brief" (Just oid)
              "what the reviewer would read is not established"
              "compose the reviewer's brief by hand"
        | otherwise -> do
            void (record "brief" (Just oid) RanHere key
              (J.explain (policyBrief (contractPolicy contract)) chosen) "admit a read-only reviewer")
            startReviewer own contract (compose key) oid

startReviewer
  :: Review Self -> Contract -> ReviewBrief -> GitOid
  -> Handler ReviewState ReviewEffects ()
startReviewer own _contract brief oid = do
  reviewer <- unfold (subgroup "review") $ child $
    withInstructions reviewerInstructions $
    withContext (selected renderBrief) $
    withModel (Literal "gpt-6-luna") $
    withEffort Medium $
    narrowed @ReviewerEffects knownEffects
      (inspectionPolicy (atRef (GitRef (renderGitOid oid))))
      ((assignment [label|review|] brief) { report = Silent })
  void (R.forwardResult reviewer (reviewSettled own))
  void (record "review" (Just oid) RanHere "reviewer_admitted"
    ("read-only luna at " <> shortOid oid) "await the verdict")

-- ---------------------------------------------------------------------------
-- The verdict seam: verdict routing. Policy: routing.
-- ---------------------------------------------------------------------------

onReview
  :: Either ResponseFailure (ResponseResult ReviewVerdict)
  -> Handler ReviewState ReviewEffects ()
onReview result = do
  own <- R.self @Review
  state <- R.get
  let contract = reviewContract state
      latest = take 1 (reverse (reviewEvidence state))
  case (result, latest) of
    (Left failure, _) -> do
      index <- record "verdict" Nothing RanHere "review_failed"
        (Text.pack (show failure)) "root decides"
      notify' Alert Nothing ("the reviewer did not settle: " <> Text.pack (show failure))
        RanHere index "admit another reviewer or accept by hand"
    (Right _, []) -> do
      index <- record "verdict" Nothing RanHere "no_candidate"
        "a verdict arrived with no candidate in state" "root decides"
      notify' Alert Nothing "a verdict arrived with no candidate"
        RanHere index "read the review's history"
    (Right receipt, evidence : _) -> case responseValue receipt of
      PremiseProblem reason -> do
        index <- record "verdict" (Just (evidenceCandidate evidence)) RanHere
          "contract_change_needed" reason "notify the root"
        notify' Alert (Just (evidenceCandidate evidence))
          ("the reviewer says the contract is wrong: " <> reason) RanHere index
          "amend the task contract, then re-admit"
      Accepted scope -> do
        void (record "verdict" (Just (evidenceCandidate evidence)) RanHere
          "accepted" scope "score the risk, then merge")
        scoreRisk contract evidence
        publishCandidate own contract evidence
      RepairRequested findings -> routeFindings own contract evidence findings

routeFindings
  :: Review Self -> Contract -> Evidence -> [Text]
  -> Handler ReviewState ReviewEffects ()
routeFindings own contract evidence findings = do
  let oid = evidenceCandidate evidence
  answer <- J.ask1
    (J.rawState (object
      [ "findings" .= findings
      , "acceptance_checklist" .= contractChecklist contract
      , "owned_paths" .= contractOwnedPaths contract
      , "diff_stat" .= evidenceStat evidence
      ]))
    (J.choice "Which statement describes `findings`?"
      ( J.alt #addresses_named_checklist_item
          "Every finding names a file inside `owned_paths` and a change that an item of `acceptance_checklist` requires." ("addresses_named_checklist_item" :: Text)
        J..| J.alt #contract_change_needed
          "At least one finding requires a change outside `owned_paths`, or a change that no item of `acceptance_checklist` asks for." "contract_change_needed"
        J..| J.alt #style_only
          "Every finding is about naming, formatting or comment wording, and none of them changes what the code does." "style_only"
        J..| J.alt #insufficient_evidence
          "`findings` is empty, or no finding names a file or a checklist item." "insufficient_evidence" ))
  case answer of
    Left failure -> doubted "verdict" (Just oid)
      ("jev unavailable: " <> Text.pack (show failure)) "read the findings yourself"
    Right chosen -> case J.settle (policyVerdict (contractPolicy contract)) chosen
        ( #addresses_named_checklist_item (\_ -> do
            void (record "verdict" (Just oid) RanHere "addresses_named_checklist_item"
              (J.explain (policyVerdict (contractPolicy contract)) chosen) "same-child repair")
            repeatedDefect own contract evidence findings)
        J..| #contract_change_needed (\_ -> do
            index <- record "verdict" (Just oid) RanHere "contract_change_needed"
              (Text.intercalate "; " findings) "notify the root"
            notify' Alert (Just oid)
              ("the repair would change the contract: " <> Text.intercalate "; " findings)
              RanHere index "amend the contract or narrow the findings")
        J..| #style_only (\_ -> do
            index <- record "verdict" (Just oid) RanHere "style_only"
              (J.explain (policyVerdict (contractPolicy contract)) chosen) "merge with a note"
            notify' Alert (Just oid)
              ("merging over style-only findings: " <> Text.intercalate "; " findings)
              RanHere index "no action needed unless you disagree"
            scoreRisk contract evidence
            publishCandidate own contract evidence)
        J..| #insufficient_evidence (\_ -> doubted "verdict" (Just oid)
            "no finding names a file or a checklist item"
            "ask the reviewer for findings that name a file and an item") ) of
      Left doubt -> doubted "verdict" (Just oid)
        (Text.pack (show doubt) <> "; " <> J.explain (policyVerdict (contractPolicy contract)) chosen)
        "read the findings yourself"
      Right (Settled action) -> action

-- ---------------------------------------------------------------------------
-- The repeat seam: repair sameness. Is this finding the same defect as the last one?
-- A repeat escalates instead of buying a third repair. Nouls, with an exit.
-- ---------------------------------------------------------------------------

repeatedDefect
  :: Review Self -> Contract -> Evidence -> [Text]
  -> Handler ReviewState ReviewEffects ()
repeatedDefect own contract evidence findings = do
  state <- R.get
  let oid = evidenceCandidate evidence
      earlier = take 1 (reverse (reviewFindings state))
  R.put state { reviewFindings = reviewFindings state ++ [findings] }
  case earlier of
    [] -> requestRepair own oid findings
    previous : _ -> do
      answer <- J.ask
        (J.rawState (object
          [ "earlier_findings" .= previous
          , "new_findings" .= findings
          , "owned_paths" .= contractOwnedPaths contract
          ]))
        ( #comparable := J.noul "Do `earlier_findings` and `new_findings` each name a specific file and a specific symbol or test?"
            :& #same := J.noul "Do `new_findings` describe the same defect in the same place as `earlier_findings`?" )
      case answer of
        Left failure -> doubted "repeat" (Just oid)
          ("jev unavailable: " <> Text.pack (show failure))
          "compare the two finding lists yourself"
        Right response -> do
          let a = J.answers response
          if a.comparable.yes < (noulFloor (contractPolicy contract))
            then doubted "repeat" (Just oid)
              "one of the finding lists names no file or symbol"
              "ask the reviewer for findings that name a file and a symbol"
            else if a.same.yes > (noulFloor (contractPolicy contract))
              then do
                index <- record "repeat" (Just oid) RanHere "repeated_defect"
                  ("same defect twice: " <> Text.intercalate "; " findings)
                  "stop repairing, notify the root"
                notify' Alert (Just oid)
                  ("the same defect came back after a repair: " <> Text.intercalate "; " findings)
                  RanHere index "take the task over, or re-scope it"
              else do
                void (record "repeat" (Just oid) RanHere "new_defect"
                  (Text.intercalate "; " findings) "same-child repair")
                requestRepair own oid findings

requestRepair
  :: Review Self -> GitOid -> [Text]
  -> Handler ReviewState ReviewEffects ()
requestRepair own oid findings = do
  state <- R.get
  let contract = reviewContract state
  if reviewRepairs state >= repairLimit (contractPolicy contract)
    then do
      index <- record "repair" (Just oid) RanHere "budget_spent"
        (Text.pack (show (reviewRepairs state)) <> " repairs already requested")
        "notify the root"
      notify' Alert (Just oid) "the repair budget for this task is spent"
        RanHere index "take the task over, or raise the budget"
    else do
      attempt <- requestWith (responseActor (reviewWorker state))
        ((assignment [label|repair|] RepairTask
            { repairTaskName = contractTask contract
            , repairCandidate = renderGitOid oid
            , repairFindings = findings
            , repairChecklist = contractChecklist contract
            })
          { guidance = Just (Text.unlines
              ("Repair your own candidate. Address exactly these findings:" : map ("  - " <>) findings))
          , report = Silent })
      void (R.forwardResult attempt (repairSettled own))
      R.modify' (\current -> current { reviewRepairs = reviewRepairs current + 1 })
      void (record "repair" (Just oid) RanHere "repair_requested"
        (Text.intercalate "; " findings) "await the revised candidate")

-- ---------------------------------------------------------------------------
-- The risk seam: risk, as a Noul battery. The review scores its own candidate;
-- the root sorts across reviews in code with `mergeOrder`.
-- ---------------------------------------------------------------------------

scoreRisk :: Contract -> Evidence -> Handler ReviewState ReviewEffects ()
scoreRisk contract evidence = do
  let oid = evidenceCandidate evidence
  answer <- J.ask
    (J.rawState (object
      [ "owned_paths" .= contractOwnedPaths contract
      , "diff_stat" .= evidenceStat evidence
      , "hunks" .= Text.take 24000 (evidenceHunks evidence)
      ]))
    ( #legible := J.noul "Does `hunks` show the changed lines for every file named in `diff_stat`?"
        :& #touches_outside_ownership := J.noul "Does `diff_stat` name a file that is not in `owned_paths`?"
        :& #changes_public_item_used_elsewhere := J.noul "Do `hunks` change the name, signature or variants of a `pub` item?"
        :& #deletes_or_weakens_test := J.noul "Do `hunks` delete a `#[test]` function or replace an assertion with a weaker one?"
        :& #leaves_todo := J.noul "Do `hunks` add or keep a `todo!()` or `unimplemented!()`?" )
  case answer of
    Left failure -> doubted "risk" (Just oid)
      ("jev unavailable: " <> Text.pack (show failure)) "rank the merges yourself"
    Right response -> do
      let a = J.answers response
      if a.legible.yes < (noulFloor (contractPolicy contract))
        then doubted "risk" (Just oid)
          "the hunks do not cover the files in the stat, so risk cannot be scored"
          "re-derive the diff, then rank the merges yourself"
        else do
          let scored =
                [ ("touches_outside_ownership", a.touches_outside_ownership.yes)
                , ("changes_public_item_used_elsewhere", a.changes_public_item_used_elsewhere.yes)
                , ("deletes_or_weakens_test", a.deletes_or_weakens_test.yes)
                , ("leaves_todo", a.leaves_todo.yes)
                ]
          R.modify' (\current -> current { reviewRisk = scored })
          void (record "risk" (Just oid) RanHere "scored"
            (Text.intercalate ", " [key | (key, yes) <- scored, yes > (noulFloor (contractPolicy contract))])
            "merge order is derived in code from these")

-- ---------------------------------------------------------------------------
-- The stuck seam: stuck detection. A progress delta with no new evidence, twice.
-- Policy: routing.
-- ---------------------------------------------------------------------------

onProgress :: ProgressState ImplNote -> Handler ReviewState ReviewEffects ()
onProgress observation = case observation of
  ProgressUpdate _ note -> do
    state <- R.get
    let unchanged = case reviewLastNote state of
          Just previous -> noteEvidence previous == noteEvidence note
          Nothing -> False
        stalls = if unchanged then reviewStalls state + 1 else 0
    R.put state { reviewLastNote = Just note, reviewStalls = stalls }
    if stalls >= 2 then askStuck note else pure ()
  _ -> pure ()

askStuck :: ImplNote -> Handler ReviewState ReviewEffects ()
askStuck note
  | Text.null (Text.strip (noteText note)) =
      doubted "stuck" Nothing "two empty progress notes in a row"
        "ask the child what it is doing"
  | otherwise = do
      policy <- currentPolicy
      answer <- J.ask1
        (J.rawState (object
          [ "progress_text" .= noteText note
          , "evidence_added_since_last_update" .= ([] :: [Text])
          ]))
        (J.choice "Which statement describes `progress_text`?"
          ( J.alt #blocked
              "`progress_text` names a command, error or missing input that it is waiting on, and no evidence was added." ("blocked" :: Text)
            J..| J.alt #working
              "`progress_text` names a step that is under way and different from the previous one." "working"
            J..| J.alt #insufficient_evidence
              "`progress_text` is a restatement of the task with no step, command or error in it." "insufficient_evidence" ))
      case answer of
        Left failure -> doubted "stuck" Nothing
          ("jev unavailable: " <> Text.pack (show failure)) "read the progress note yourself"
        Right chosen -> case J.settle (policyStuck policy) chosen
            ( #blocked (\_ -> do
                index <- record "stuck" Nothing ChildReported "blocked"
                  (noteText note) "notify the root"
                notify' Alert Nothing
                  ("the child looks blocked: " <> noteText note) ChildReported index
                  "message the child, or take the task over")
            J..| #working (\_ -> void (record "stuck" Nothing ChildReported "working"
                (noteText note) "keep waiting"))
            J..| #insufficient_evidence (\_ -> doubted "stuck" Nothing
                "the progress note names no step, command or error"
                "ask the child for a concrete step") ) of
          Left doubt -> doubted "stuck" Nothing
            (Text.pack (show doubt) <> "; " <> J.explain (policyStuck policy) chosen)
            "read the progress note yourself"
          Right (Settled action) -> action

-- ---------------------------------------------------------------------------
-- Merge and the integrated check, through the merge actor
-- ---------------------------------------------------------------------------

publishCandidate :: Review Self -> Contract -> Evidence -> Handler ReviewState ReviewEffects ()
publishCandidate own contract evidence = do
  MergeTarget { mergeActor = target } <- R.gets reviewMerge
  let oid = evidenceCandidate evidence
  outcome <- R.call (publish (R.client target)) Merge.PublishRequest
    { Merge.publishTask = contractTask contract
    , Merge.publishCandidate = oid
    , Merge.publishMessage = "Merge " <> contractTask contract <> " " <> shortOid oid
    }
  case outcome of
    MergeFailed detail -> do
      index <- record "merge" (Just oid) RanHere "merge_error" detail "notify the root"
      notify' Alert (Just oid) ("the merge failed: " <> detail) RanHere index
        "merge by hand in the integration worktree"
    Conflict reason paths -> do
      index <- record "merge" (Just oid) RanHere "conflict"
        (reason <> ": " <> Text.intercalate ", " paths) "notify the root"
      notify' Alert (Just oid)
        ("merge conflict in " <> Text.intercalate ", " paths) RanHere index
        "resolve in the integration worktree, then tell the review"
    Blocked reason -> do
      index <- record "merge" (Just oid) RanHere "integration_blocked" reason "notify the root"
      notify' Alert (Just oid) ("integration is blocked: " <> reason) RanHere index
        "fix the worktree and branch, R.send (reconcile (R.client mergeActor)) note, then re-request"
    Published checked _ check -> do
      R.modify' (\current -> current { reviewMerged = Just checked, reviewCheck = Just check })
      index <- record "check" (Just oid) RanHere "merged_green"
        ("integrated head " <> shortOid checked <> "; " <> checkDetail check) "report completion"
      notify' Info (Just oid) ("merged and green at " <> shortOid checked)
        RanHere index "no reply needed"
    RedRolledBack checked before check -> do
      R.modify' (\current -> current { reviewCheck = Just check })
      index <- record "check" (Just oid) RanHere "red_rolled_back"
        ("checked " <> shortOid checked <> ", worktree back on " <> shortOid before
          <> ": " <> checkDetail check)
        "request repair carrying the check output"
      notify' Alert (Just oid)
        ("the integrated check failed at " <> shortOid checked <> " and was rolled back: "
          <> checkDetail check)
        RanHere index "the review is requesting the repair; confirm or take it over"
      requestRepair own oid
        ["the integrated check failed at " <> shortOid checked <> " (rolled back to "
          <> shortOid before <> "): " <> checkDetail check]
