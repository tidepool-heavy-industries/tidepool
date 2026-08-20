{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | The recursive companion: one root turn in which a coalgebra window
-- finalizes a 'ThoughtF' layer (or a local finish) for its own node only,
-- each branch descends recursively from its parent's FROZEN context, an
-- algebra window folds typed results in declared branch order (including a
-- leaf's, which sees a childless layer), and the operator gets a folded
-- answer with the tree inspectable but not primary.
--
-- Recursion lives in this authored outer loop, never inside a window: a
-- fork\/fanout child compiles against its parent's row MINUS @Fork@, so it is
-- structurally incapable of producing grandchildren.
--
-- Cognition enters at exactly two windows — 'discover' (branched off the
-- parent's frozen post-coalgebra context) and 'foldNode' (forked) — every
-- other function here is compiled coordination.
--
-- @Companion@ is @M@ at the driver's outer row (@RunLLMTurn@, @AskUser@,
-- @Console@, @Worktree@, @RepoEvent@, @Exec@, @Subagent@, and @Journal@ —
-- exactly the driver's widened outer session,
-- @selfharness::driver::outer_decls@, the same row @dev-tree@ compiles
-- against); this file declares no new effect. @tidepool-harness
-- \/tests\/dogfood_harness_typecheck.rs@ pins it against that row.
--
-- __Worktree coordination (PRD 21 lane C5).__ 'mergeFold' is authored
-- policy over @Worktree@\/@Exec@\/@Subagent@, run by 'foldAt' — never by a
-- window: node windows ('layerWindow'\/'foldWindow') compile against the
-- answerer's narrow row, which has neither. It reaches git the same way
-- @dev-tree@'s own @mergeChild@ does (mechanical @git@ through @Exec@ in a
-- worktree this node owns; see 'gitIn'), never through a runtime workflow
-- verb — @tidepool-worktree@'s own boundary is unchanged, and
-- @tidepool_worktree::merge@ is the fast-tier-tested ground truth this
-- mirrors.
module Harness
  ( -- * The locked entry points
    State (..)
  , Config (..)
  , RunSummary (..)
  , initialState
  , render
  , loop
  , resumeLoop

    -- * The driver's own vocabulary
  , Companion
  , Folded
  , NodeSeed (..)
  , NodePath (..)
  , NodeAnswer (..)
  , traverseLayer

    -- * What a window may finalize
  , LayerProposal (..)
  , ProposedBranch (..)
  , Posture (..)
  , ProposedStrategy (..)
  , BranchRoleWire (..)
  , ProposedEditWire (..)
  , FoldDecision (..)
  , FoldOutcome (..)

    -- * The gate
  , GatePolicy (..)
  , GateVerdict (..)
  , LayerApproval (..)

    -- * Pure decisions (no model, no operator — exercised directly)
  , layerFromProposal
  , applyGate
  , renderPath
  , childPath
  , slug
  , childSeed
  , childEdge
  , childAllowance

    -- * The merge fold (PRD 21 C5 — pure decisions exercised directly)
  , mergePlan
  , MergeStatus (..)
  , mergeNote
  ) where

import Data.List (find)
import qualified Data.List as L
import Data.List.NonEmpty (NonEmpty ((:|)))
import qualified Data.List.NonEmpty as NE
import Data.Maybe (catMaybes)
import qualified Data.Text as T
import GHC.Generics (Generic)
import HarnessTypes
import Tidepool.Agent.Spawn (renderSpawnError, spawnAgent)
import Tidepool.Aeson (FromJSON, ToJSON, Value, object, toJSON, (.=))
import Tidepool.Aeson.Schema (JsonSchema)
import Tidepool.Effects
  ( ContextRef
  , ExecError (..)
  , InvocationExit
  , WorktreeHandle (..)
  , WorktreeReceipt (..)
  , freezeContext
  , renderInvocationExit
  , runIn
  , runLLMTurnBranchLabeled
  , runLLMTurnFork
  , say
  , spawnSpecIn
  , takeDelegatedBranches
  )
import Tidepool.Form (askUser)
import Tidepool.Harness (Harness)
import Tidepool.Journal (record)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import Tidepool.Resume (ResumeFold)
import Tidepool.Swarm (cyclesToInt, mkCycles, splitAllowance)
import Tidepool.Thought (Coalg, Strategy, ThoughtF, depthCapped, fanOutCapped, thoughtHylo)
import qualified Tidepool.Thought as Th
import Tidepool.Worktree

-- | The orchestration monad.  @Harness@ is @M@ under a friendlier name; the
-- row it resolves to is the driver's outer session.
type Companion = Harness

-- | The hylo's @b@: a node's fold, waiting for the ONE thing an algebra
-- cannot otherwise learn — its own identity.
--
-- @ThoughtF@ has no @task@ slot (@Tidepool.Swarm@'s @PlanF@ does), so an
-- algebra never sees the seed its coalgebra saw.  Threading the node's
-- identity down as a READER is the answer, and it is what keeps 'thoughtHylo'
-- verbatim rather than forked into a second recursion engine that carries a
-- seed alongside.
type Folded = NodePath -> Companion NodeAnswer

-- ---------------------------------------------------------------------------
-- The seed — and why it lives HERE rather than in "HarnessTypes"
--
-- A 'ContextRef' is declared by @RunLLMTurn@'s own decl, so it EXISTS only in
-- a row containing that effect.  The answerer's row does not
-- (@[AskUser, Fork, ReadState, Finalize T]@), and "HarnessTypes" has to stay
-- compilable there or the window types it defines become unnameable by the
-- windows asked to finalize them.  So the seed and every pure decision over
-- it live beside 'loop', exactly as @dev-tree@ keeps its own @NodeSeed@
-- (which carries a @WorktreeHandle@) beside its own.  They are still PURE and
-- still EXPORTED — @dev-tree@'s @resumePlanFor@\/@childAllowance@ are the
-- precedent — so a test drives them directly with no model and no operator.
-- ---------------------------------------------------------------------------

-- | What a node needs in order to be unfolded.  The hylo's @a@.
data NodeSeed = NodeSeed
  { seedPath      :: NodePath
  , seedBrief     :: Th.ForkBrief
  , seedDepth     :: Int
  , -- | Node allowance for THIS SUBTREE, including this node.  Spent
    -- STRUCTURALLY: a node reserves one unit for itself and divides the
    -- remainder among its children ('childAllowance'), so the run's total is
    -- bounded with no mutable counter and completion order cannot reach it.
    --
    -- 'Tidepool.Thought.nodeCapped' is the pure-fixture spelling of the same
    -- cap, but it is @MonadState Int m@ and the outer row is not a
    -- @MonadState@ stack — hence the seed-carried form, exactly as
    -- @dev-tree@'s @childAllowance@ carries agent cycles.
    seedAllowance :: Int
  , -- | The frozen context this node's coalgebra window is BRANCHED FROM
    -- ('discover') — PRD 21 locked decision 2, made real: a child forks its
    -- parent's frozen post-coalgebra window rather than reading a rendered
    -- summary of it, so the shared prefix is the ancestor's actual transcript
    -- and only the divergent suffix is new.
    --
    -- __It is a CAPABILITY, threaded as a value only.__  It never enters
    -- 'State' (which is checkpointed JSON), is never interpolated into a
    -- prompt, and is never reconstructed from text — possession is
    -- permission, and a ref only ever comes from @freezeContext@ or from a
    -- @runLLMTurnBranch@'s own return.  @ContextRef@ has 'Show' and 'Eq', so
    -- the seed's deriving clause is unaffected — but 'Show' is not a channel:
    -- nothing renders a seed into a prompt, a journal payload, or the receipt,
    -- and nothing should start.
    seedRef       :: ContextRef
  }
  deriving (Show, Eq)

-- | Divide what is left after this node's own reservation among its children.
--
-- Routed through 'Tidepool.Swarm.splitAllowance' rather than a second,
-- hand-rolled formula: that function's conservation law (property-tested —
-- kept plus every child's share never exceeds the input) is what
-- 'Harness.applyGate'\'s @Add@ path leans on to redivide a layer for a NEW
-- branch count without minting allowance the parent never had.  Writing the
-- arithmetic twice is how that class of bug recurs.
--
-- Conservative on purpose (a subtree finishing under its share does not
-- return the remainder to its siblings) and NEVER clamped upward: a share
-- that floors to zero stays zero, so a node that cannot fund every child
-- hands the underfunded ones nothing rather than minting nodes the parent
-- does not have.  'allowanceCapped' turns that zero into a typed
-- @BudgetForced ForcedNodeCount@ finish for the child.
childAllowance :: NodeSeed -> Int -> Int
childAllowance parent n
  | n <= 0 = 0
  | otherwise = case snd (splitAllowance (mkCycles parent.seedAllowance) (mkCycles 1) n) of
      [] -> 0
      (perChild : _) -> cyclesToInt perChild

-- | THE inheritance seam — the ONE function that decides what a child knows
-- from above, and the answer is now: its parent's own frozen window.
--
-- @parent.seedRef@ read here is NOT the ref the parent branched from.
-- 'discover' re-stamps the seed with the ref its OWN coalgebra window froze
-- before building this layer, so what a child forks is its parent's
-- POST-coalgebra context — the decision it just made included.  'childEdge'
-- is the paired constructor that also stamps the matching 'Th.Branch'.
childSeed :: NodeSeed -> Int -> Int -> Th.ForkBrief -> NodeSeed
childSeed parent allowance i b =
  NodeSeed
    { seedPath = childPath parent.seedPath i b.title
    , seedBrief = b
    , seedDepth = parent.seedDepth + 1
    , seedAllowance = allowance
    , seedRef = parent.seedRef
    }

-- | THE single producer of a child's whole identity: a 'Th.Branch' (what the
-- render and every receipt read) paired with the 'NodeSeed' that same
-- child's OWN window is prompted from ('seedBrief' — a coalgebra sees only
-- its own seed, never the 'Th.Branch' that names it).  Both halves are built
-- from the same 'Th.ForkBrief' HERE, in one call, so they cannot desync —
-- the exact failure class a prior regression pinned: a brief written on the
-- 'Th.Branch' and not into the seed renders right and works wrong.
--
-- Every caller that mints OR edits a branch of @parent@ routes through this:
-- the original split ('splitLayer'), and every accepted gate verdict
-- ('applyGate').  A KEPT branch is rebuilt here too, from @parent@ and its
-- own current brief\/allowance, rather than patched onto its old 'Th.Branch'
-- — there is exactly one formula for "this position, this brief, this
-- allowance, this parent", never a second one a future edit path could drift
-- from.
--
-- Not made the sole way to construct a 'NodeSeed' AT THE TYPE LEVEL — that
-- would need 'NodeSeed' opaque, which ripples into every typecheck\/
-- regression probe that builds one by record literal
-- (@tidepool-harness\/tests\/dogfood_harness_typecheck.rs@).  This is the
-- documented sole producer for the two real call sites, with the fields
-- still exported.
childEdge :: NodeSeed -> Int -> Int -> Th.ForkBrief -> Th.Branch NodeSeed
childEdge parent allowance i b = Th.Branch b (childSeed parent allowance i b)

-- ---------------------------------------------------------------------------
-- The resident cycle
-- ---------------------------------------------------------------------------

-- | One resident turn discovers a tree and folds it.
--
-- POLICY IS MIDDLEWARE, composed by ordinary function application over
-- @Tidepool.Thought@'s own @Coalg -> Coalg@ combinators.  Read the coalgebra
-- OUTSIDE-IN, which is also the order the caps FIRE:
--
-- 1. 'gatedLayer' sees the finished layer last, so the operator is never
--    shown a layer a budget already refused (design doc §6).  This is why the
--    gate is the OUTERMOST wrapper rather than the innermost one the doc's §1
--    sketch draws: @fanOutCapped@ decides AFTER its inner coalgebra ran, so a
--    gate beneath it would present a layer the fan-out cap then discards.
-- 2. 'fanOutCapped' refuses the DESCENT of a too-wide layer — the coalgebra's
--    own work already happened, because fan-out is a property of the produced
--    layer, not of the seed.
-- 3. 'depthCapped' refuses BEFORE the window runs.
-- 4. 'allowanceCapped' refuses BEFORE the window runs, on the seed-carried
--    node budget (see 'NodeSeed'\'s @seedAllowance@).
--
-- TWO PASSES, deliberately.  'thoughtHylo' returns a 'Folded' — every
-- coalgebra window has run by then, and no algebra window has.  Applying it
-- at @NodePath []@ runs the folds.  Neither ORDER changes: discoveries happen
-- in declared branch order (@traverse@ preserves it) and folds happen in
-- declared branch order bottom-up ('traverseLayer' preserves it).  Completion
-- order is not an input to anything here.
--
-- THE ROOT'S REF IS MINTED HERE, and that is what makes 'discover' uniform.
-- @freezeContext@ freezes the CALLING window — this loop's own accumulated
-- context, which is precisely what the root's coalgebra should fork from —
-- so the root enters the hylo holding a ref exactly like every descendant
-- does, and 'discover' has no root special case to get wrong.
loop :: State -> Companion State
loop st
  -- The SEED GATE (operator decision, 2026-08-19): an empty question means
  -- no operator has chosen one yet, so the loop's first act is to ask —
  -- BEFORE any model window runs.  This is deliberately a SHORT cycle
  -- (ask, store, return): the answerer framing for a cycle is rendered
  -- from the state the cycle STARTED with, so running the tree in the
  -- same cycle would run every window under a framing whose question is
  -- still blank.  The seeded question is 'State', so it checkpoints, and
  -- every later loop skips straight past this guard.  A blank submission
  -- re-asks (bounded by the driver's consecutive-re-presentation cap).
  | T.strip st.question == "" = do
      say "No question is seeded yet — provide the question this run should investigate."
      sq <- askUser @SeedQuestion
      case T.strip sq.seedQuestion of
        "" -> loop st
        q -> do
          -- Without this line the operator's next screen is the bare
          -- between-loops "loop complete" gate — reading as if a run
          -- happened and produced nothing (dogfood finding, 2026-08-19).
          say [fmt|Question seeded: {q}
Nothing has run yet — press Continue to start the first turn.|]
          pure st {question = q}
loop st = do
  record "turn" rootKey (object ["root" .= st.question, "config" .= toJSON cfg])
  rootRef <- freezeContext
  f <- thoughtHylo (foldNode cfg st.draft) coalg (rootSeed st rootRef)
  answer <- f (NodePath [])
  record
    "turn"
    rootKey
    ( object
        [ "answer" .= answer.answerSynthesis
        , "nodes" .= answer.answerNodes
        , "windows" .= answer.answerWindows
        ]
    )
  -- The turn's outcome, ON the operator page (dogfood finding, 2026-08-19):
  -- without this the parked between-loops screen says only "loop complete" —
  -- the operator sat 37 minutes next to a finished answer they couldn't see.
  say
    [fmt|Turn {show (st.turnCount + 1)} complete — {show answer.answerNodes} nodes, {show answer.answerWindows} windows.

{answer.answerSynthesis}

Start the next turn when ready — optionally with steering.|]
  pure
    st
      { turnCount = st.turnCount + 1
      , lastRun = Just (summarize answer)
      , draft = answer.answerDraft
      }
  where
    cfg = st.config
    rootKey = renderPath (NodePath [])
    coalg =
      journaled
        ( gatedLayer
            cfg
            ( fanOutCapped
                seedDepth
                cfg.maxFanOut
                (depthCapped seedDepth cfg.maxDepth (allowanceCapped (discover cfg)))
            )
        )

-- | The honest opt-out ('Tidepool.Resume' module doc): this harness's
-- 'record' calls exist for the durable transcript, not to replay prior
-- windows on a resumed boot — a rerun re-derives 'draft'\/'lastRun' from
-- 'State' the same way a fresh run does, so there is nothing here for a
-- fold of recorded steps to inject.  Declaring this (rather than leaving it
-- absent) is what turns a journal-bearing crash recovery from a boot
-- refusal into an ordinary 'loop' call.
resumeLoop :: ResumeFold -> State -> Companion State
resumeLoop _fold = loop

-- | The root's own seed.  Its brief IS the operator's question, so the root
-- window is asked the same shape of thing every descendant is — and it holds
-- a real 'ContextRef' (the loop's own frozen prefix, minted in 'loop') for
-- the same reason: the root is not a special case anywhere below it.
rootSeed :: State -> ContextRef -> NodeSeed
rootSeed st ref =
  NodeSeed
    { seedPath = NodePath []
    , seedBrief = Th.ForkBrief "root" Th.Primary st.question
    , seedDepth = 0
    , seedAllowance = st.config.maxNodes
    , seedRef = ref
    }

summarize :: NodeAnswer -> RunSummary
summarize a =
  RunSummary
    { runAnswer = a.answerSynthesis
    , runTensions = a.answerTensions
    , runTree = subtreeLines "root" a
    , runNodes = a.answerNodes
    , runWindows = a.answerWindows
    , runForced = a.answerForced
    , runFailed = a.answerFailed
    }

-- ---------------------------------------------------------------------------
-- The two windows — ONE named seam each
--
-- Gap 3 (a window's abnormal exit aborting its siblings) is closed at the
-- verb: @runLLMTurnFork \@T@ answers @M (Either InvocationExit T)@, so the
-- DRIVER no longer fails the whole outer turn when one branch's window
-- exhausts its rounds — the exit arrives as data at that branch's position
-- (plans/self-iterating-harness/21-c3-exit-verb.md).
--
-- Both seams pass the @Either@ through UNWRAPPED, and their two callers fold
-- it — because the two exits mean different things and the difference is the
-- whole point:
--
-- * a COALGEBRA exit means this node decided no layer, so 'discover' makes it
--   a leaf whose @FinishOrigin@ is @InvocationFailed@.  The node's own algebra
--   then folds it like any other childless layer.  The verb wraps the WHOLE
--   returned pair — a window that never finalized minted no context of its own
--   to hand on — which is also why nothing there needs a ref it does not have.
-- * an ALGEBRA exit means the layer was fine and the FOLD failed.  'foldAt'
--   replaces only what this node itself owed (its synthesis and tensions) and
--   rolls its children's answers, tree lines, and accounting up untouched.
--   Discarding them would erase completed sibling work one level up, which is
--   the same erasure decision 6 forbids at a branch position.
--
-- Neither exit is an abort, and neither is silent: both journal under kind
-- @failed@, tagged with which window produced them.
--
-- TWO functions rather than one @runWindow :: Text -> Companion a@, and the
-- reason is mechanical: extract's typed-yield site pass rejects a
-- @runLLMTurnFork \@a@ call at a bare type VARIABLE ("polymorphic runLLMTurn
-- site"), so a polymorphic wrapper cannot exist.  They stay adjacent, and
-- they are the only two call sites.
--
-- Neither is plain @runLLMTurn@: the plain form lands on the driver's ONE
-- reused per-loop answerer node, which accumulates every hole's exchange into
-- a single flat context — that would put every sibling's output into every
-- later node's window, exactly what locked decision 2 forbids.  Both forms
-- below mint a fresh answerer node per window.
-- ---------------------------------------------------------------------------

-- | The COALGEBRA's window, BRANCHED off @ref@ — a fresh child window whose
-- shared prefix is the frozen context that ref names, never an empty root.
-- It returns its answer AND its own post-finalize ref, which is what lets the
-- next layer down branch off THIS node ('childSeed').
--
-- Labeled with this node's own rendered 'NodePath' (PRD 21 C5 GUI lane) —
-- @root@ for the root window, @root\/1-x@ etc. for a descendant — so the
-- per-node operator GUI can register and route this window's own asks/notes
-- to its own panel instead of the default one. The label rides the wire
-- structurally, never parsed back out of the prompt.
layerWindow ::
  NodePath -> ContextRef -> Text -> Companion (Either InvocationExit (LayerProposal, ContextRef))
layerWindow path ref prompt = runLLMTurnBranchLabeled @LayerProposal (renderPath path) ref prompt

-- | The ALGEBRA's window, FORKED — deliberately NOT branched, for two
-- reasons, and both are load-bearing.
--
-- (1) POLICY.  PRD 21 says the algebra's model window gets a RENDERED view of
-- the realized layer by default ('algebraPrompt' builds it).  Mounting the
-- live value into the window instead is the escalation PRD open question 3
-- gates, explicitly out of v1.
--
-- (2) MECHANISM — it could not branch even if the policy said to.  A branch
-- needs a 'ContextRef', and the only ref a node ever mints is the one
-- 'discover' gets back from ITS OWN coalgebra window.  @ThoughtF@ has no task
-- slot (@Tidepool.Swarm@'s @PlanF@ does), so nothing carries a per-node value
-- from a node's coalgebra to its own algebra: 'foldNode' only ever sees
-- @ThoughtF Folded@.  This is the same wall the gate-count receipt hit (see
-- @HarnessTypes.render@) — the only relay is stamping the ref into every
-- child's seed and reading it back off a child's answer, which would be a
-- capability laundered through the fold and would still be wrong for a leaf,
-- whose layer has no children to read it off at all.
foldWindow :: Text -> Companion (Either InvocationExit FoldDecision)
foldWindow prompt = runLLMTurnFork @FoldDecision prompt

-- ---------------------------------------------------------------------------
-- The coalgebra — how to split
-- ---------------------------------------------------------------------------

-- | Unfold one node: ONE window, then the pure conversion, then the journal.
-- Every policy that could refuse this is middleware wrapped around it at the
-- 'loop' call site, so what is left here is only what splitting MEANS.
--
-- UNIFORM ACROSS ROOT AND DESCENDANTS, with no special case: every seed
-- reaching here holds a ref (the root's from @freezeContext@ in 'loop', a
-- child's from its parent's own window), so every coalgebra window is a
-- genuine fork off a frozen prefix.
--
-- @myRef@ is this node's OWN post-coalgebra context, and re-stamping the seed
-- with it before 'layerFromProposal' is what makes every child seed carry it
-- ('childSeed' reads @parent.seedRef@).  That is locked decision 2 exactly:
-- a child forks the frozen context of its parent's window as of the moment
-- that parent decided this layer.
discover :: Config -> NodeSeed -> Companion (ThoughtF NodeSeed)
discover cfg seed = do
  outcome <- layerWindow seed.seedPath seed.seedRef (coalgebraPrompt cfg.maxFanOut seed)
  -- What the WINDOW said, before any policy could refuse or amend it. Kind
  -- 'proposed', never 'split' — see 'journaled' for why the two are different
  -- entries rather than one.
  record "proposed" (renderPath seed.seedPath) (proposedPayload outcome)
  pure $ case outcome of
    -- A window that exited without an answer decided no layer, so this node
    -- becomes a leaf whose ORIGIN says why (PRD 21 locked decision 6: folded
    -- as data at its branch position, never an exception that erases
    -- siblings).  It also minted no context of its own — which is exactly why
    -- the verb wraps the whole pair, and why nothing here needs a ref it does
    -- not have: a 'Th.Finish' has no children to seed.
    Left e -> invocationFailed seed (renderInvocationExit e)
    Right (proposal, myRef) -> layerFromProposal seed {seedRef = myRef} proposal

proposedPayload :: Either InvocationExit (LayerProposal, ContextRef) -> Value
proposedPayload outcome = case outcome of
  Left e -> object ["exit" .= renderInvocationExit e]
  Right (ProposeFinish {localAnswer = t}, _) -> object ["finish" .= t]
  Right (p@ProposeSplit {}, _) ->
    object
      [ "posture" .= show p.splitPosture
      , "focus" .= p.splitFocus
      , "strategy" .= show p.splitStrategy
      , "branches" .= map (.branchTitle) p.splitBranches
      ]

-- | The seed-carried node-count cap.
--
-- 'Tidepool.Thought.nodeCapped' is the same cap over @MonadState Int@, and
-- the outer row is not a @MonadState@ stack — so the allowance rides on the
-- seed instead: a node reserves one unit for itself and divides the remainder
-- among its children ('childAllowance').  Same shape, same determinism
-- guarantee, and completion order cannot reach it.
allowanceCapped :: Coalg Companion NodeSeed -> Coalg Companion NodeSeed
allowanceCapped inner seed
  | seed.seedAllowance < 1 =
      pure
        ( Th.Finish
            (Th.Draft "<node-count cap reached>" (Th.BudgetForced Th.ForcedNodeCount) seed.seedDepth)
        )
  | otherwise = inner seed

-- | The operator's authority over the driver, raised by the AUTHORED LOOP
-- between the caps and the descent.
--
-- It CANNOT live inside a window: @drive_fanout_child_inner@ services
-- @finalize@ only, and a nested @askUser@ from a fanout child gets a clear
-- error naming the gap rather than service.  That placement is also right on
-- the merits — the gate is the operator's authority over the driver, not a
-- capability the model holds.
--
-- 'GateOff' raises no suspension at all: 'gateApplies' is False and the layer
-- passes straight through.
gatedLayer :: Config -> Coalg Companion NodeSeed -> Coalg Companion NodeSeed
gatedLayer cfg inner seed = do
  layer <- inner seed
  if gateApplies cfg.gatePolicy layer
    then do
      say [fmt|{renderPath seed.seedPath} proposes {show (Th.branchCount layer)} branches — gate open|]
      gateRounds cfg seed 0 layer
    else pure layer

-- | Present the layer until the operator approves it, or until
-- @gateMaxRounds@ presentations have happened — at which point the layer
-- proceeds AS LAST AMENDED and the fact that the bound was hit is journaled,
-- never swallowed.
--
-- 'applyGate' is pure and total, so every verdict's effect on the layer is a
-- function a test drives directly; this loop is only the presentation and the
-- record of it.  A refused verdict (a title no branch has, an emptied layer)
-- re-presents the SAME layer.
gateRounds :: Config -> NodeSeed -> Int -> ThoughtF NodeSeed -> Companion (ThoughtF NodeSeed)
gateRounds cfg seed done layer
  | done >= cfg.gateMaxRounds = do
      record "gate" key (boundPayload done)
      pure layer
  | otherwise = do
      say (gateLayerNote seed.seedPath layer)
      approval <- askUser @LayerApproval
      let rounds = done + 1
      case applyGate seed approval layer of
        Left why -> do
          record "gate" key (gatePayload approval rounds ("refused — " <> why))
          say [fmt|gate refused that verdict: {why}|]
          gateRounds cfg seed rounds layer
        Right amended -> do
          record "gate" key (gatePayload approval rounds "applied")
          case approval.gateVerdict of
            Approve -> pure amended
            _ -> gateRounds cfg seed rounds amended
  where
    key = renderPath seed.seedPath
    boundNote :: Text
    boundNote =
      [fmt|the gate bound ({show cfg.gateMaxRounds} rounds) was hit; the layer proceeds as last amended|]
    boundPayload rounds =
      object
        [ "verdict" .= ("<bound reached>" :: Text)
        , "target" .= ("" :: Text)
        , "note" .= boundNote
        , "rounds" .= rounds
        ]
    gatePayload a rounds outcome =
      object
        [ "verdict" .= show a.gateVerdict
        , "target" .= a.gateTarget
        , "note" .= (a.gateNote <> " (" <> outcome <> ")")
        , "rounds" .= rounds
        ]

-- | What the operator is judging, posted to the note feed right before every
-- gate round's form.  The form itself is shape-derived (@askUser
-- \@LayerApproval@) and so carries no context — without this note the
-- operator is shown six bare fields and asked to judge a layer they cannot
-- see (dogfood finding, 2026-08-19).  Rendered from the CURRENT layer each
-- round, so a round following an Amend\/Prune\/Add shows the layer as
-- amended, not as first proposed.
gateLayerNote :: NodePath -> ThoughtF NodeSeed -> Text
gateLayerNote path layer =
  [fmt|{renderPath path} proposes:
{branchLines}

Verdicts — Approve: run the branches as shown (all other fields ignored).
Prune: remove the branch whose exact title is in "gate target".
Amend: rewrite the instruction of the branch titled "gate target" to "gate text".
Add: append a new branch built from "gate title" + "gate role" + "gate text".
"gate note" is journaled alongside any verdict.|]
  where
    branchLines =
      T.intercalate
        "\n"
        [ [fmt|{show i}. {br.brief.title} ({show br.brief.role}) — {br.brief.instruction}|]
        | (i, br) <- zip [(1 :: Int) ..] (layerBranches layer)
        ]

-- ---------------------------------------------------------------------------
-- The pure decisions — no model, no operator, no capability USED
--
-- Both are total functions a test calls directly.  They live here rather than
-- in "HarnessTypes" only because they mention 'NodeSeed', which carries a
-- 'ContextRef' (see the seed's own section above); they THREAD that ref and
-- never read it.
-- ---------------------------------------------------------------------------

-- | Turn what a coalgebra window finalized into the layer the driver descends
-- through.  Total, pure, and the whole shape-validation story: a test calls
-- it directly with no model anywhere in the path.
--
-- Three outcomes:
--
-- * 'ProposeFinish' → a 'Th.Finish' the model chose;
-- * a well-formed 'ProposeSplit' → the matching posture, branch seeds built
--   by 'childSeed', in declared order;
-- * an EMPTY branch list, or a branch with a blank title or instruction →
--   @Finish (Draft why (InvocationFailed …))@.  A split that declares no
--   branches is not a finish the model chose; it is a window that failed to
--   produce a usable layer, and @FinishOrigin@ is where that distinction
--   already lives.  The algebra folds it as ordinary data.
--
-- The seed it is handed is the one 'discover' re-stamped with this node's own
-- post-coalgebra ref, so the child seeds it builds fork the window that just
-- produced this very proposal.
--
-- Note what the layer CARRIES: the model's PROPOSED strategy, unmodified.
-- The transformation to 'Th.Sequential' happens at the descent
-- ('traverseLayer'), which is the one place that can also stamp it.
layerFromProposal :: NodeSeed -> LayerProposal -> ThoughtF NodeSeed
layerFromProposal seed proposal = case proposal of
  ProposeFinish {localAnswer = t} -> Th.Finish (Th.Draft t Th.ModelFinished seed.seedDepth)
  ProposeSplit
    { splitPosture = po
    , splitFocus = f
    , splitStrategy = ps
    , splitBranches = pbs
    } -> splitLayer seed po f ps pbs

splitLayer :: NodeSeed -> Posture -> Text -> ProposedStrategy -> [ProposedBranch] -> ThoughtF NodeSeed
splitLayer seed po f ps pbs
  | null pbs = invocationFailed seed "the window proposed a split with no branches"
  | any blank pbs = invocationFailed seed "the window proposed a branch with a blank title or instruction"
  | otherwise = postureLayer po f (NE.fromList (imap child pbs)) (strategyOfWire ps)
  where
    blank pb = strip pb.branchTitle == "" || strip pb.branchInstruction == ""
    allowance = childAllowance seed (length pbs)
    child i pb =
      childEdge seed allowance i (Th.ForkBrief pb.branchTitle (roleOf pb.branchRole) pb.branchInstruction)

invocationFailed :: NodeSeed -> Text -> ThoughtF NodeSeed
invocationFailed seed why =
  Th.Finish (Th.Draft why (Th.InvocationFailed (Th.NodeFailure why)) seed.seedDepth)

-- | Apply one operator verdict to a proposed layer.  PURE and TOTAL, so a
-- test drives every verdict with no operator and no model.
--
-- * 'Approve' — identity.
-- * 'Prune' — drop the branch whose title matches 'gateTarget'.  Pruning the
--   LAST branch is REFUSED: every @FinishOrigin@ names either the model's
--   choice, a budget, or an invocation failure, and an operator emptying a
--   layer is none of those.  The harness does not invent a fourth or borrow a
--   wrong one — the operator who wants the subtree gone prunes to one branch,
--   or ends the turn.
-- * 'Amend' — replace the matching branch's instruction.
-- * 'Add' — append a branch built from the form's title\/role\/text, and
--   REDIVIDE every kept-plus-added branch's allowance for the new branch
--   count ('reshared').  Appending a branch changes the count @seed@'s
--   allowance was originally split by, so a survivor's OLD per-child share
--   is stale the instant the layer grows — handing the new branch a copy of
--   it (or leaving the survivors' shares untouched) mints allowance @seed@
--   never had to give out.
--
-- @seed@ is the NODE THIS LAYER BELONGS TO — the parent whose own
-- 'NodeSeed.seedAllowance' every branch's share is divided out of
-- ('childAllowance'), and the same parent every kept-or-edited branch is
-- rebuilt against via 'childEdge' — never a sibling's already-built seed.
--
-- A verdict naming a title no branch has is 'Left', and the caller
-- re-presents the form.  Every accepted verdict re-derives the surviving
-- branches' paths from their NEW positions ('childPath'), so ids stay dense
-- and ordered and still agree with what the algebra will compute from the
-- same branch list.
--
-- A 'Th.Finish' is returned unchanged: the gate is only ever presented for a
-- split ('gateApplies'), and being total about it is cheaper than being
-- partial.
applyGate :: NodeSeed -> LayerApproval -> ThoughtF NodeSeed -> Either Text (ThoughtF NodeSeed)
applyGate seed approval layer = case layer of
  Th.Finish _ -> Right layer
  _ -> case approval.gateVerdict of
    Approve -> Right layer
    Prune
      | not (any titled brs) -> Left (noSuchBranch approval.gateTarget)
      | length brs <= 1 -> Left "pruning the last branch would leave the layer empty"
      | otherwise -> reindexed [(br.brief, br.value.seedAllowance) | br <- filter (not . titled) brs]
    Amend
      | not (any titled brs) -> Left (noSuchBranch approval.gateTarget)
      | otherwise -> reindexed [(amendedBrief br, br.value.seedAllowance) | br <- brs]
    Add
      | strip approval.gateTitle == "" -> Left "an added branch needs a title"
      | null brs -> Left "a layer with no branches cannot take an added one"
      | otherwise -> reshared (map (.brief) brs <> [addedBrief])
  where
    brs = layerBranches layer
    titled br = br.brief.title == approval.gateTarget
    noSuchBranch t = [fmt|no branch in this layer is titled "{t}"|]
    amendedBrief br = if titled br then updated else br.brief
      where updated = br.brief {Th.instruction = approval.gateText}
    addedBrief = Th.ForkBrief approval.gateTitle (roleOf approval.gateRole) approval.gateText
    -- Every branch this layer ends up with is minted fresh from @seed@ via
    -- 'childEdge' — never patched onto an old 'Th.Branch' — so a kept
    -- branch's brief and its own seed's prompted brief cannot drift, the
    -- same guarantee 'childEdge's own doc explains.
    --
    -- 'reindexed': allowance UNCHANGED per branch — 'Prune' and 'Amend' never
    -- change the branch COUNT @seed@'s allowance was divided by, so each
    -- kept branch's existing share is still exactly what 'splitLayer'
    -- computed for it.
    reindexed briefsWithAllowance = case NE.nonEmpty (imap mkEdge briefsWithAllowance) of
      Nothing -> Left "an edited layer cannot be rebuilt with zero branches"
      Just ne -> Right (rebuildLayer layer ne)
      where
        mkEdge i (b, allowance) = childEdge seed allowance i b
    -- 'reshared': every kept-plus-added brief is redivided through
    -- 'childAllowance' (itself routed through 'Tidepool.Swarm.splitAllowance')
    -- for the NEW branch count — the fix for the review's minted-allowance
    -- scenario: allowance 5, two children at 2 each; an operator 'Add'
    -- redivides all THREE to 1 each rather than handing the third a bare
    -- copy of a sibling's now-stale 2.
    reshared briefs = case NE.nonEmpty (imap mkEdge briefs) of
      Nothing -> Left "an edited layer cannot be rebuilt with zero branches"
      Just ne -> Right (rebuildLayer layer ne)
      where
        n = length briefs
        mkEdge i b = childEdge seed (childAllowance seed n) i b

-- ---------------------------------------------------------------------------
-- The algebra — how to combine
-- ---------------------------------------------------------------------------

-- | Fold one node.  The design doc's @fold@; spelled 'foldNode' only to stay
-- clear of the wholesale @Control.Lens@ re-export in the prelude.
--
-- It returns a 'Folded' — a fold WAITING for its own path — rather than a
-- finished answer, because that is what lets 'thoughtHylo' stay verbatim: the
-- algebra has no seed, so the node's identity arrives from its parent, which
-- computes it with the very same 'childPath' the coalgebra used.
--
-- @initialDraft@ is the companion's working draft AS OF TURN START
-- ('loop''s own @st.draft@) — ONE frozen value shared by every node's own
-- fold, never threaded bottom-up between siblings or levels (PRD 21 lane
-- C4: "applies them to a known snapshot"; keeping that snapshot the SAME
-- one at every node is what keeps every fold's own apply independent of
-- fold ORDER, the same guarantee 'traverseLayer' already gives the rest of
-- this driver). Only the ROOT's own resulting 'answerDraft' is ever read
-- back into 'State' ('loop'); every other node's is informational,
-- demonstrating the same mechanism at its own position.
foldNode :: Config -> Text -> ThoughtF Folded -> Companion Folded
foldNode _cfg initialDraft layer = pure (foldAt initialDraft layer)

-- ---------------------------------------------------------------------------
-- The merge fold — PRD 21 lane C5, "Worktree coordination"
--
-- Worktrees are LAZY: a node acquires one only at its first real merge
-- need — today, its first content-bearing child ('mergePlan''s whole job).
-- The design also names a node's OWN subagent spawn as an acquisition
-- trigger; that rides through the very same 'answerMergeBranch' channel
-- once a node window has a route to produce one (a sibling lane's — no new
-- vocabulary needed here, per the PRD's "two edit channels" bullet). A
-- purely deliberative node — no child carried a branch — never touches git
-- at all, which is what keeps a run with no worktree content anywhere
-- byte-behaviorally unchanged.
--
-- Merges run in DECLARED branch order (locked decision 5: the same order
-- the algebra receives results) — 'kids' is already that order, so this
-- only ever walks it once, left to right. A conflict never leaves this
-- node's worktree mid-merge: 'mergeChildInto' reads the conflicting paths
-- and aborts before it ever returns, mirroring
-- @tidepool_worktree::merge@'s own discipline (its fast-tier tests are the
-- ground truth for this same "merge / conflict-and-abort / failure"
-- semantics). A child whose conflict the resolver could not settle is
-- simply not merged — its own worktree stays retained (PRD 19 retain-first;
-- nothing here ever deletes anything) and the fold continues to the NEXT
-- child rather than aborting the whole node. That is the DROP half of
-- "drop, re-propose, escalate to the operator is the algebra's choice"
-- (decision 6): the runtime never escalates on its own initiative — it
-- reports, in the algebra's own prompt ('algebraPrompt''s merge block and
-- the journal's @"merge"@ entries), and the model decides what (if
-- anything) to say about it. GUI and cancellation are parked (PRD 21 C5
-- context note 4): "the algebra deciding" is just this typed data flowing
-- through the existing fold, nothing more, for now.
-- ---------------------------------------------------------------------------

-- | Declared-order plan: which of a realized layer's children carry
-- mergeable content, in the order they were declared. 'Nothing' IS the
-- lazy-acquisition gate — no content-bearing child means this node never
-- creates a worktree at all. Pure and total, exercised directly with no
-- git and no agent anywhere in the path.
mergePlan :: [Maybe Text] -> Maybe (NonEmpty Text)
mergePlan = NE.nonEmpty . catMaybes

-- | THIS node's own delegation branches, runtime-stamped and read back at
-- fold time — the landing of the doc above's "a node's OWN subagent spawn
-- as an acquisition trigger". 'takeDelegatedBranches' is a plain read
-- against driver-owned state, keyed by this node's OWN rendered
-- 'NodePath' (the ONLY identity a delegating branch child's window and
-- this node's own fold are guaranteed to agree on — see
-- @tidepool-harness@'s @engine::parse_companion_node_path@): the model's
-- 'LayerProposal'/'FoldDecision' are never consulted, and could not
-- influence this even if they tried, since neither type has anywhere to
-- put a branch name. Ordered oldest-first; a node whose coalgebra never
-- delegated reads back @[]@, byte-identical to before this landed.
--
-- Multiple delegations from ONE node's own coalgebra (a do-block calling
-- @delegate@ more than once) are a real, decided case, not an oversight:
-- LAST completed wins for 'answerMergeBranch' — every earlier one is still
-- a real committed worktree/branch, just not the one this node hands up —
-- and every one of them is journaled together under kind @"delegate"@ so
-- none is silently lost, only silently not selected.
ownDelegatedBranch :: NodePath -> Companion (Maybe Text)
ownDelegatedBranch path = do
  branches <- takeDelegatedBranches (renderPath path)
  case branches of
    [] -> pure Nothing
    [one] -> pure (Just one)
    many -> do
      -- Non-empty by construction (the `[]`/`[one]` cases above are
      -- exhaustively handled), so this is `L.last`'s sanctioned deliberate
      -- spelling, not an unguarded partial use.
      let winner = L.last many
      record
        "delegate"
        (renderPath path)
        (object ["branches" .= many, "used" .= winner])
      pure (Just winner)

-- | One child branch's merge outcome. Never a panic and never a
-- half-merged tree: 'mergeChildInto' always restores a clean worktree
-- before returning either conflict arm.
data MergeStatus
  = MergeClean
  | MergeResolved Text
  | MergeConflicted Text
  deriving (Show, Eq)

-- | @branch: merged cleanly@ / @branch: conflict resolved by an agent —
-- notes@ / @branch: NOT merged — why@ — one line per merge step, read by
-- both the algebra's prompt and a human tailing the journal. Pure and
-- total, exercised directly.
mergeNote :: Text -> MergeStatus -> Text
mergeNote branchText status = case status of
  MergeClean -> [fmt|{branchText}: merged cleanly|]
  MergeResolved notes -> [fmt|{branchText}: conflict resolved by an agent — {notes}|]
  MergeConflicted why -> [fmt|{branchText}: NOT merged — {why}|]

-- | What the merge-resolution agent finalizes — decision 6's typed resolver
-- outcome, in the RUNTIME's own vocabulary (never the algebra's): either a
-- trivial conflict resolved and committed, or a report of why it is not.
-- Trivial-vs-nontrivial is the RESOLVER's own judgment, never a heuristic
-- here — 'resolveConflict' only reads back what actually happened to
-- @HEAD@, never trusting 'resolved' alone.
data MergeResolution = MergeResolution
  { resolved        :: Bool
  , resolutionNotes :: Text
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | Fold every content-bearing child's branch into this node's own
-- worktree, lazily acquired on first need, in declared order — PLUS, when
-- this node's OWN coalgebra window delegated, its own branch (declared
-- FIRST: it is this node's own contribution, the same position a leaf's
-- own new edit proposal always resolves at, one level up). Returns this
-- node's own resulting branch (to hand up to ITS parent, via
-- 'answerMergeBranch') and one human-legible note per merge step, for the
-- algebra's prompt and the journal.
--
-- __The lazy-acquisition gate now has TWO shapes, not one.__ A node with
-- no own delegation and no content-bearing children never creates a
-- worktree at all (silent @Nothing@\/@[]@, byte-identical to before this
-- landed). A node whose ONLY content is its own delegation hands that
-- branch up DIRECTLY — no wrapping worktree, no merge notes, no @"merge"@
-- journal entry — because the delegated cycle's bound worktree already IS
-- the acquisition; creating a second one just to merge one branch into it
-- would be pure overhead. Only when there is more than one branch to fold
-- together (an own delegation ALONGSIDE content-bearing children) does this
-- create a worktree and merge, exactly as before.
mergeFold :: NodePath -> Maybe Text -> [Th.Branch NodeAnswer] -> Companion (Maybe Text, [Text])
mergeFold path ownBranch kids = case (ownBranch, mergePlan (map ((.answerMergeBranch) . (.value)) kids)) of
  (Nothing, Nothing) -> pure (Nothing, [])
  (Just b, Nothing) -> pure (Just b, [])
  (mbOwn, Just (first_ :| rest)) ->
    createWorktree (fromCurrentRepository (renderPath path)) >>= \case
      Left err -> pure (Nothing, [[fmt|worktree acquisition failed: {renderWorktreeError err}|]])
      Right tree -> do
        notes <- traverse (mergeStep tree) (maybe id (:) mbOwn (first_ : rest))
        branchText <- renderBranchName <$> worktreeBranch tree
        pure (Just branchText, notes)
  where
    mergeStep t branchText = mergeNote branchText <$> mergeChildInto t branchText

-- | One merge, mechanical first: 'gitIn' running plain @git merge --no-ff@
-- in a worktree this node owns — authored policy, not a runtime workflow
-- verb (PRD 19's freeze, unchanged). A real conflict is read and aborted
-- BEFORE the resolver ever spawns, so the resolver always starts from a
-- clean worktree.
mergeChildInto :: WorktreeHandle -> Text -> Companion MergeStatus
mergeChildInto tree branchText =
  gitIn tree [fmt|merge --no-ff -m "fold {branchText}" {branchText}|] >>= \case
    Left e -> pure (MergeConflicted [fmt|merge could not run: {e}|])
    Right pr
      | ok pr -> pure MergeClean
      | otherwise -> handleConflict tree branchText

-- | Read the conflicting paths, abort (restoring a clean worktree), THEN
-- spawn the resolver — never the other order, so the resolver's own
-- worktree access is never mid-merge.
handleConflict :: WorktreeHandle -> Text -> Companion MergeStatus
handleConflict tree branchText = do
  paths <-
    gitIn tree "diff --name-only --diff-filter=U" >>= \case
      Left _ -> pure []
      Right pr -> pure (filter (/= "") (T.lines pr.stdout))
  _ <- gitIn tree "merge --abort"
  resolveConflict tree branchText paths

-- | One ephemeral agent, bound to this node's own (now clean again)
-- worktree, asked to redo the merge and resolve the conflict itself.
-- Repository events are authoritative, never the agent's own summary
-- (mirrors @dev-tree@'s own rule): @HEAD@ either side of the cycle is what
-- actually decides 'MergeResolved' vs 'MergeConflicted', not 'resolved'
-- alone — an agent that claims success without ever moving @HEAD@ is
-- reported as unresolved, the same way a leaf worker that claims
-- completion without committing is caught in @dev-tree@'s own ladder.
resolveConflict :: WorktreeHandle -> Text -> [Text] -> Companion MergeStatus
resolveConflict tree branchText paths = do
  before <- worktreeHead tree
  spawnAgent @MergeResolution
    (spawnSpecIn (worktreeId tree) (branchText <> "-merge-resolve") (mergeResolutionPrompt branchText paths))
    >>= \case
      Left err ->
        pure (MergeConflicted [fmt|resolver spawn failed: {renderSpawnError err}; conflicting: {T.intercalate ", " paths}|])
      Right (_, mr) -> do
        after <- worktreeHead tree
        pure $
          if mr.resolved && after /= before
            then MergeResolved mr.resolutionNotes
            else
              MergeConflicted $
                if mr.resolved
                  then [fmt|resolver claimed success but HEAD never moved; conflicting: {T.intercalate ", " paths}|]
                  else [fmt|{mr.resolutionNotes}; conflicting: {T.intercalate ", " paths}|]

mergeResolutionPrompt :: Text -> [Text] -> Text
mergeResolutionPrompt branchText paths =
  [fmt|Merge the branch `{branchText}` into this worktree's current branch:

`git merge --no-ff {branchText}`

It conflicts on: {T.intercalate ", " paths}. Resolve the conflicts by editing
the conflicted files, `git add` your resolution, and `git commit` to finish
the merge. If the conflict is not trivially resolvable, leave the merge
unresolved and say why instead of guessing at intent.

Finalize a MergeResolution {{ resolved, resolutionNotes }}.|]

-- | Plain git in a worktree this node owns — authored policy, not a
-- runtime workflow verb (mirrors @dev-tree@'s own @gitIn@ verbatim).
gitIn :: WorktreeHandle -> Text -> Companion (Either Text Proc)
gitIn tree args =
  runIn tree.handleReceipt.cwd ("git " <> args) >>= \case
    Left e -> pure (Left (renderExecError e))
    Right pr -> pure (Right pr)

renderExecError :: ExecError -> Text
renderExecError e = case e of
  ExecSpawn detail -> "could not spawn: " <> detail
  ExecBadDir detail -> "bad working directory: " <> detail

-- | Every field a fold's own 'FoldOutcome' contributes — replacing the
-- pre-refactor positional 7-tuple with a named record (companion review
-- step 4) so the two sibling lanes extending 'foldAt' (delegate-effect,
-- node-worktree-tree) have named fields to grow rather than a tuple
-- position to renumber every time either one touches this seam.
data FoldOutcome = FoldOutcome
  { outcomeSynthesis :: Text
  , outcomeTensions  :: [Text]
  , -- | This node's own artifact pool to hand its PARENT: its own new
    -- proposals, PLUS — endorsement propagation (companion review step 4)
    -- — every artifact this node's own fold ENDORSED (selected via
    -- 'foldEditsInOrder') from its children, republished upward under
    -- their ORIGINAL id. An artifact this fold did not select is dropped
    -- from this route permanently, never re-offered further up.
    outcomeArtifacts :: [Th.Artifact Text]
  , -- | The renderable content behind every EDIT id in 'outcomeArtifacts'
    -- — see 'answerArtifactRenders''s own doc.
    outcomeRenders   :: [(Th.ArtifactId, Text)]
  , outcomeReceipts  :: [Th.EditReceipt]
  , outcomeBadges    :: [Text]
  , outcomeFailed    :: Int
  , outcomeDraft     :: Text
  }

foldAt :: Text -> ThoughtF Folded -> Folded
foldAt initialDraft layer path = do
  realized <- traverseLayer (layerStrategy layer) (applyChild path) (indexLayer layer)
  let kids = layerBranches realized
      kidAnswers = map (.value) kids
      -- The pool THIS node's own fold may select from: every immediate
      -- child's own advertised artifacts — never a grandchild's DIRECTLY (a
      -- grandchild's artifact reaches this pool only if the immediate
      -- child's OWN fold already endorsed and republished it — endorsement
      -- propagation, companion review step 4) and never this node's own (a
      -- node cannot select what it has not proposed yet — decision 7:
      -- "approval is the PARENT's fold").
      pool = concatMap (.answerArtifacts) kidAnswers
      poolRenders = concatMap (.answerArtifactRenders) kidAnswers
      isRoot = path == NodePath []
  ownBranch <- ownDelegatedBranch path
  (mergeBranch, mergeNotes) <- mergeFold path ownBranch kids
  case mergeNotes of
    [] -> pure ()
    _ -> record "merge" key (object ["branch" .= mergeBranch, "steps" .= mergeNotes])
  outcome0 <- foldWindow (algebraPrompt path layer kids pool poolRenders initialDraft isRoot mergeNotes)
  outcome <- resolveWithOneRetry path layer kids pool poolRenders initialDraft isRoot mergeNotes outcome0
  -- A FOLD window that exits is this node's OWN failure, and it must not be
  -- its subtree's.  The children below it already ran and already folded;
  -- discarding their answers, their tree lines, or their accounting here
  -- would erase completed sibling work one level up — the same erasure
  -- decision 6 forbids at a branch position, just reached from the algebra
  -- side.  So the exit replaces only what this node itself was going to
  -- contribute (its synthesis and tensions), and everything the children
  -- earned rolls up untouched.  A window that exits proposes and selects
  -- nothing — same as a Narrative fold that mentions neither.
  fo <- case outcome of
    Right fd -> resolveAndApply path isRoot initialDraft pool poolRenders fd
    Left e ->
      pure
        FoldOutcome
          { outcomeSynthesis =
              [fmt|<this node's fold window exited: {renderInvocationExit e}> — its {show (length kids)} branch result(s) are below, unfolded|]
          , outcomeTensions = []
          , outcomeArtifacts = []
          , outcomeRenders = []
          , outcomeReceipts = []
          , outcomeBadges = ["fold failed"]
          , outcomeFailed = 1
          , outcomeDraft = initialDraft
          }
  record
    "fold"
    key
    ( object
        [ "synthesis" .= fo.outcomeSynthesis
        , "tensions" .= fo.outcomeTensions
        , "children" .= map (renderPath . (.answerPath)) kidAnswers
        , "depth" .= pathDepth path
        ]
    )
  -- Two independent failure classes, journaled separately on purpose: the
  -- COALGEBRA's (this node never decided a layer — 'failureReason' reads it
  -- off the layer's own origin) and the ALGEBRA's (the layer was fine, the
  -- fold window exited).  A node can carry both.
  case failureReason layer of
    Nothing -> pure ()
    Just why -> record "failed" key (object ["reason" .= why, "window" .= ("coalgebra" :: Text)])
  case outcome of
    Right _ -> pure ()
    Left e ->
      record "failed" key (object ["reason" .= renderInvocationExit e, "window" .= ("algebra" :: Text)])
  -- The receipts (PRD 21 lane C4 step 4): emitted ONLY when this node's own
  -- fold actually approved something, joining the SAME journal vocabulary
  -- 'record' already gives "fold"/"finish"/"gate"/"failed" — a narrative
  -- fold that never selects or proposes anything journals nothing new here,
  -- which is what keeps a no-edits run's journal byte-identical to before.
  -- @status@ is "applied" ONLY at the root — every other node ran the SAME
  -- apply mechanically (against the same frozen 'initialDraft'), but its
  -- result is a PREVIEW: only the root's own selection is the turn's one
  -- real, persisted application (endorsement propagation, step 4).
  case fo.outcomeReceipts of
    [] -> pure ()
    _ ->
      record
        "edits"
        key
        ( object
            [ "before" .= initialDraft
            , "after" .= fo.outcomeDraft
            , "status" .= (if isRoot then "applied" else "preview" :: Text)
            , "receipts" .= map receiptJson fo.outcomeReceipts
            ]
        )
  pure
    NodeAnswer
      { answerPath = path
      , answerPosture = layerPosture layer
      , answerSynthesis = fo.outcomeSynthesis
      , answerTensions = fo.outcomeTensions
      , answerBadges = strategyBadges (layerStrategy layer) <> originBadges layer <> fo.outcomeBadges
      , answerTree = concatMap childLines kids
      , answerNodes = 1 + sum (map (.answerNodes) kidAnswers)
      , answerWindows = selfWindows layer + sum (map (.answerWindows) kidAnswers)
      , answerForced = selfForced layer + sum (map (.answerForced) kidAnswers)
      , answerFailed = selfFailed layer + fo.outcomeFailed + sum (map (.answerFailed) kidAnswers)
      , answerArtifacts = fo.outcomeArtifacts
      , answerArtifactRenders = fo.outcomeRenders
      , answerDraft = fo.outcomeDraft
      , answerMergeBranch = mergeBranch
      }
  where
    key = renderPath path
    -- The ONE place a child's identity is computed on the fold side, and it
    -- is 'childPath' — the same function the coalgebra used to build the
    -- child's seed, over the same branch list in the same order carrying the
    -- same brief title.  The two cannot disagree.
    applyChild p (Th.Branch b (i, f)) = f (childPath p i b.title)
    childLines (Th.Branch b a) = subtreeLines b.title a

-- ---------------------------------------------------------------------------
-- Validation caps (companion review step 6b) — module constants, never
-- silently truncated against: a proposal or payload past either cap is
-- REFUSED whole and journaled, so the record always shows the model's own
-- real ask rather than a runtime-edited version of it.
-- ---------------------------------------------------------------------------

-- | The most NEW edit proposals one fold window's own 'foldProposed' may
-- carry. A fold naming more than this many is not being asked to WORK, it
-- is spamming the draft; proposals past the cap are refused individually
-- (in submission order) and journaled as overflow.
maxProposalsPerFold :: Int
maxProposalsPerFold = 4

-- | The character-length cap on a single edit's payload — an 'AppendEdit'
-- 's @editAppend@, or a 'ReplaceOnce''s @editNeedle@ plus @editReplacement@
-- combined. An oversize proposal is refused WHOLE, never truncated: a
-- truncated edit is silently a different edit than the one proposed.
maxEditPayloadChars :: Int
maxEditPayloadChars = 4000

-- | The C4 mechanism itself, run over one node's own (possibly
-- corrective-retried) 'FoldDecision': resolve the selection against the
-- pool, approve, apply against 'initialDraft', stamp this node's own new
-- proposals, and republish every artifact this fold ENDORSED back into
-- 'outcomeArtifacts' under its original id — routed straight through
-- 'Th.resolveSelection'\/'Th.approve'\/'Th.applyEdits'\/'Th.approvedOrder'
-- rather than reimplemented (PRD 21 lane C4's landed, property-tested
-- mechanism).
--
-- 'Th.decisionProposed' is left @[]@ on the 'Th.FoldDecision' built for
-- 'Th.resolveSelection': that function reads only 'Th.selected'\/
-- 'Th.composition' (@Tidepool.Thought@'s own definition), so THIS node's own
-- new proposals never resolve against themselves here — they become
-- available to select only at the PARENT's fold, exactly like a child's
-- (stamped below, via 'stampProposed').
resolveAndApply ::
  NodePath ->
  Bool ->
  Text ->
  [Th.Artifact Text] ->
  [(Th.ArtifactId, Text)] ->
  FoldDecision ->
  Companion FoldOutcome
resolveAndApply path isRoot initialDraft pool poolRenders fd = do
  (newArtifacts, newRenders) <- stampProposed path isRoot fd.foldProposed
  classifySelectionJournal path pool fd.foldEditsInOrder
  case Th.approve product_ of
    Nothing ->
      pure
        FoldOutcome
          { outcomeSynthesis = fd.foldSynthesis
          , outcomeTensions = fd.foldTensions
          , outcomeArtifacts = newArtifacts
          , outcomeRenders = newRenders
          , outcomeReceipts = []
          , outcomeBadges = []
          , outcomeFailed = 0
          , outcomeDraft = initialDraft
          }
    Just approved ->
      let (nodeDraft, receipts) = Th.applyEdits id approved initialDraft
          receiptList = NE.toList receipts
          Th.CompositionOrder endorsedIds = Th.approvedOrder approved
          endorsed = [a | aid <- endorsedIds, Just a <- [find ((== aid) . Th.artifactId) pool]]
          endorsedRenders = [(aid, r) | aid <- endorsedIds, Just r <- [lookup aid poolRenders]]
       in pure
            FoldOutcome
              { outcomeSynthesis = fd.foldSynthesis
              , outcomeTensions = fd.foldTensions
              , outcomeArtifacts = newArtifacts <> endorsed
              , outcomeRenders = newRenders <> endorsedRenders
              , outcomeReceipts = receiptList
              , outcomeBadges = [editsBadge isRoot receiptList]
              , outcomeFailed = 0
              , outcomeDraft = nodeDraft
              }
  where
    decision =
      Th.FoldDecision
        fd.foldSynthesis
        (map Th.ArtifactId fd.foldEditsInOrder)
        (Th.CompositionOrder (map Th.ArtifactId fd.foldEditsInOrder))
        []
    product_ = Th.resolveSelection decision pool

-- | The corrective-retry mechanism (companion review step 6c): when a fold
-- names a NONEMPTY 'foldEditsInOrder' containing an id this node's pool
-- cannot honor — unavailable (unknown, or evidence-only, hence not
-- selectable) or duplicated — give the model exactly ONE fresh attempt
-- before falling through to applying whatever DOES resolve. The retry is a
-- genuinely SECOND window ('foldWindow' again, a fresh @runLLMTurnFork@),
-- never a patched-up reuse of the first. Capped at one: whatever the retry
-- returns (even if still invalid) is taken as final — 'resolveAndApply'
-- always applies the valid, unique subset regardless, and
-- 'classifySelectionJournal' records exactly what happened to every id.
resolveWithOneRetry ::
  NodePath ->
  ThoughtF a ->
  [Th.Branch NodeAnswer] ->
  [Th.Artifact Text] ->
  [(Th.ArtifactId, Text)] ->
  Text ->
  Bool ->
  [Text] ->
  Either InvocationExit FoldDecision ->
  Companion (Either InvocationExit FoldDecision)
resolveWithOneRetry path layer kids pool poolRenders draft isRoot mergeNotes outcome0 = case outcome0 of
  Left _ -> pure outcome0
  Right fd0
    | null fd0.foldEditsInOrder -> pure outcome0
    | null unavailable && null duplicated -> pure outcome0
    | otherwise -> do
        record
          "retry"
          (renderPath path)
          (object ["reason" .= ("invalid selection" :: Text), "unavailable" .= unavailable, "duplicated" .= duplicated])
        foldWindow (correctiveRetryPrompt path layer kids pool poolRenders draft isRoot mergeNotes unavailable duplicated fd0)
    where
      (unavailable, duplicated) = invalidSelectionIds pool fd0.foldEditsInOrder

-- | The two ways a selected id can fail to validate: named but not a
-- selectable edit in the pool (unknown entirely, or an evidence artifact —
-- both "unavailable" from the model's point of view), or named more than
-- once. Each list is deduped for display; the presence of EITHER is what
-- 'resolveWithOneRetry' treats as needing a retry.
invalidSelectionIds :: [Th.Artifact Text] -> [Text] -> ([Text], [Text])
invalidSelectionIds pool ids = (L.nub (filter (not . isSelectable) ids), L.nub (ids L.\\ L.nub ids))
  where
    isSelectable i = any (\a -> isEditArtifact a && artifactIdText (Th.artifactId a) == i) pool

isEditArtifact :: Th.Artifact s -> Bool
isEditArtifact a = case a of
  Th.EditArtifact {} -> True
  Th.EvidenceArtifact {} -> False

-- | Journal every id in a fold's (possibly corrective-retried) final
-- 'foldEditsInOrder', with the SAME per-id reason a human reading the
-- journal would want: @resolved@ (a real edit plan, applied or attempted),
-- @unknown-id@ (nothing in the pool has this id), @evidence-id@ (present,
-- but not selectable — an 'Th.EvidenceArtifact'), or @duplicate@ (a REPEAT
-- occurrence — the first occurrence of a given id gets its own real
-- reason; only the second and later ones are "duplicate"). Silent when
-- 'foldEditsInOrder' is empty, matching this module's "nothing to say,
-- nothing journaled" convention elsewhere.
classifySelectionJournal :: NodePath -> [Th.Artifact Text] -> [Text] -> Companion ()
classifySelectionJournal _ _ [] = pure ()
classifySelectionJournal path pool ids =
  record "selection" (renderPath path) (object ["ids" .= map toEntry (classifySelection pool ids)])
  where
    toEntry (i, reason) = object ["id" .= i, "reason" .= (reason :: Text)]

classifySelection :: [Th.Artifact Text] -> [Text] -> [(Text, Text)]
classifySelection pool = go []
  where
    go _ [] = []
    go seen (i : rest)
      | i `elem` seen = (i, "duplicate") : go seen rest
      | otherwise = (i, reasonFor i) : go (i : seen) rest
    reasonFor i = case find ((== i) . artifactIdText . Th.artifactId) pool of
      Nothing -> "unknown-id"
      Just a | isEditArtifact a -> "resolved"
      Just _ -> "evidence-id"

-- | Stamp this node's own wire-level proposals into real, id-bearing
-- 'Th.Artifact's — the RUNTIME half of decision 7 ("intent metadata + an
-- @s -> Either EditFailure s@ plan"; ids are the runtime's to assign, never
-- the model's). Ids are derived from the node's own path plus a dense
-- per-node index over the SURVIVING proposals only, never from
-- model-produced text — the same containment discipline 'slug' gives node
-- ids, and the same reason a refused proposal consumes no id and leaves no
-- gap. The real closure itself ('wrapEdit') is built HERE, in the runtime,
-- never by a window: neither window this harness ever finalizes across can
-- carry one (see 'ProposedEditWire''s own doc).
--
-- Every refusal — a blank intent, a blank\/no-op payload, an oversize
-- payload, overflow past 'maxProposalsPerFold', or (new in endorsement
-- propagation, step 4) ANY proposal at all from the ROOT fold, which
-- selects but never authors — is journaled under kind @refused@, naming
-- the node path, the proposal's 1-based SUBMITTED ordinal (so the record
-- still shows which of the model's own proposals it was), and the reason.
-- A refused proposal never becomes an 'Th.Artifact' and never enters a
-- pool.
stampProposed :: NodePath -> Bool -> [ProposedEditWire] -> Companion ([Th.Artifact Text], [(Th.ArtifactId, Text)])
stampProposed path isRoot = go (1 :: Int) (0 :: Int)
  where
    go _ _ [] = pure ([], [])
    go ordinal kept (p : rest) = case validateProposal isRoot kept p of
      Left why -> do
        record "refused" (renderPath path) (object ["ordinal" .= ordinal, "reason" .= why])
        go (ordinal + 1) kept rest
      Right () -> do
        let aid = artifactIdAt path (kept + 1)
        (arts, renders) <- go (ordinal + 1) (kept + 1) rest
        pure
          ( Th.EditArtifact aid (Th.EditIntent (editIntentOf p)) (wrapEdit p) : arts
          , (aid, renderEditContent p) : renders
          )

-- | Every reason a proposal is refused before it ever reaches an id
-- (companion review steps 4 + 6a): root proposing at all, a blank intent, a
-- blank\/no-op payload (an empty append, or a replace whose needle is blank
-- or whose replacement is identical to its needle), overflow past
-- 'maxProposalsPerFold', or a payload past 'maxEditPayloadChars'. Checked
-- in this fixed order so the journaled reason is always the FIRST one that
-- applies, never an arbitrary pick among several that all hold.
validateProposal :: Bool -> Int -> ProposedEditWire -> Either Text ()
validateProposal isRoot keptCount p
  | isRoot = Left "the root fold may not propose new edits — select from the pool instead"
  | strip (editIntentOf p) == "" = Left "an edit with a blank intent is refused"
  | keptCount >= maxProposalsPerFold =
      Left [fmt|more than {show maxProposalsPerFold} proposals in one fold — the rest are refused, not truncated|]
  | payloadChars > maxEditPayloadChars =
      Left [fmt|edit payload exceeds the {show maxEditPayloadChars}-character cap ({show payloadChars} chars)|]
  | noOpPayload = Left "an edit with no actual change is refused"
  | otherwise = Right ()
  where
    payloadChars = case p of
      AppendEdit {editAppend = a} -> T.length a
      ReplaceOnce {editNeedle = n, editReplacement = r} -> T.length n + T.length r
    noOpPayload = case p of
      AppendEdit {editAppend = a} -> strip a == ""
      ReplaceOnce {editNeedle = n, editReplacement = r} -> strip n == "" || n == r

editIntentOf :: ProposedEditWire -> Text
editIntentOf p = case p of
  AppendEdit {editIntent = i} -> i
  ReplaceOnce {editIntent = i} -> i

-- | The exact content a proposal would write, for the pool listing an
-- approving fold reads (companion review DEFECT 1's fix) — an
-- 'AppendEdit''s appended text, or a 'ReplaceOnce''s needle AND replacement
-- both, each in its own delimited block so the boundaries of what will
-- change are unambiguous.
renderEditContent :: ProposedEditWire -> Text
renderEditContent p = case p of
  AppendEdit {editAppend = a} ->
    [fmt|--- BEGIN APPEND ---
{a}
--- END APPEND ---|]
  ReplaceOnce {editNeedle = n, editReplacement = r} ->
    [fmt|--- NEEDLE ---
{n}
--- REPLACEMENT ---
{r}
--- END ---|]

artifactIdAt :: NodePath -> Int -> Th.ArtifactId
artifactIdAt path i = Th.ArtifactId (renderPath path <> "#" <> show i)

artifactIdText :: Th.ArtifactId -> Text
artifactIdText (Th.ArtifactId t) = t

-- | The RUNTIME's own closure over an already-validated wire-level proposal
-- (every blank/no-op/oversize/root-forbidden case is refused earlier, at
-- 'stampProposed' — this is total over what survives that check).
-- 'Th.appendWithSeparator'\/'Th.replaceExactlyOnce' are the two edit
-- vocabulary's own pure bodies (companion review step 7), shared with
-- "Tidepool.Thought"'s own property tests — this is dispatch, not a second
-- implementation.
wrapEdit :: ProposedEditWire -> (Text -> Either Th.EditFailure Text)
wrapEdit p s = case p of
  AppendEdit {editAppend = a} -> Right (Th.appendWithSeparator s a)
  ReplaceOnce {editNeedle = n, editReplacement = r} -> Th.replaceExactlyOnce n r s

-- | @["edits: N applied, M refused"]@ at the ROOT, or
-- @["edits: N previewed, M refused"]@ everywhere else — the wording
-- endorsement propagation needs (companion review step 4): every node ran
-- the SAME apply mechanically, but only the root's is the turn's one real,
-- persisted application. Only reached when 'outcomeReceipts' is
-- non-empty; matches 'strategyBadges' and 'originBadges''s own "silent when
-- nothing to say" shape.
editsBadge :: Bool -> [Th.EditReceipt] -> Text
editsBadge isRoot receipts =
  [fmt|edits: {show applied} {verb}, {show refused} refused|]
  where
    applied = length (rights (map (.receiptOutcome) receipts))
    refused = length receipts - applied
    verb = if isRoot then "applied" :: Text else "previewed"

-- | One 'Th.EditReceipt' as the journal's own @object@ vocabulary — built by
-- hand, like every other 'record' payload in this module, rather than a
-- generic 'ToJSON' derive: 'Th.EditReceipt' carries no such instance (it is
-- not one of this harness's model-facing wire types), and nothing else here
-- needs it to.
receiptJson :: Th.EditReceipt -> Value
receiptJson r =
  object
    [ "artifact" .= artifactIdText r.receiptArtifact
    , "intent" .= editIntentText r.receiptIntent
    , "before" .= r.receiptBefore
    , "outcome" .= case r.receiptOutcome of
        Left f -> object ["refused" .= f.editFailureReason]
        Right t -> object ["applied" .= t]
    ]
  where
    editIntentText (Th.EditIntent t) = t

-- | THE CONCURRENCY SEAM — the ONE place this harness visits a layer's
-- children.
--
-- Today it is ordinary order-preserving sequential traversal and it IGNORES
-- its 'Strategy' argument except that the caller stamps the transformation
-- ('strategyBadges'); a model that proposes @Concurrent@ gets @Sequential@
-- execution, EXPLICITLY (PRD 21 locked decision 9), never a silent downgrade.
--
-- When the green-threads lane lands, the authored loop gains
-- async\/mapConcurrently over the outer row and subtree-level concurrency
-- drops in HERE, by interpreting 'Strategy' at the descent — without touching
-- the fanout machinery at all.  Branch order is preserved by construction
-- either way, so nothing downstream changes.
--
-- (The DISCOVERY descent is @thoughtHylo@'s own @traverse@, inside
-- @Tidepool.Thought@, which this lane consumes verbatim.  Making discovery
-- concurrent is that module's edit, not this one's.)
traverseLayer :: Strategy -> (Th.Branch a -> Companion b) -> ThoughtF a -> Companion (ThoughtF b)
traverseLayer _proposed visit layer = case layer of
  Th.Finish d -> pure (Th.Finish d)
  Th.Explore f bs s -> Th.Explore f <$> traverse step bs <*> pure s
  Th.Compare d os s -> Th.Compare d <$> traverse step os <*> pure s
  Th.Challenge c as s -> Th.Challenge c <$> traverse step as <*> pure s
  where
    step br = Th.Branch br.brief <$> visit br

-- | How many MODEL windows this node itself spent.  A node a budget refused
-- before its coalgebra ran spent only its fold; every other node — including
-- one the fan-out cap refused, whose coalgebra HAD run — spent two.
selfWindows :: ThoughtF a -> Int
selfWindows layer = case layer of
  Th.Finish d -> case d.draftOrigin of
    Th.BudgetForced Th.ForcedDepth -> 1
    Th.BudgetForced Th.ForcedNodeCount -> 1
    _ -> 2
  _ -> 2

selfForced :: ThoughtF a -> Int
selfForced layer = case layer of
  Th.Finish d -> case d.draftOrigin of
    Th.BudgetForced _ -> 1
    _ -> 0
  _ -> 0

selfFailed :: ThoughtF a -> Int
selfFailed layer = case layer of
  Th.Finish d -> case d.draftOrigin of
    Th.InvocationFailed _ -> 1
    _ -> 0
  _ -> 0

failureReason :: ThoughtF a -> Maybe Text
failureReason layer = case layer of
  Th.Finish d -> case d.draftOrigin of
    Th.InvocationFailed f -> Just f.failureReason
    _ -> Nothing
  _ -> Nothing

originBadges :: ThoughtF a -> [Text]
originBadges layer = case layer of
  Th.Finish d -> [renderOrigin d]
  _ -> []

-- ---------------------------------------------------------------------------
-- The journal — write-only, one entry per node event, keyed by NodePath
--
-- 'record' is write-only here and no resume is built: PRD 20 S1-L5 is a
-- different lane and PRD 21's persistence section explicitly defers durable
-- branch resume.  The kinds below are still chosen so a future resume fold
-- has what it needs — a @split@ entry names its children's PATHS, which is
-- the only durable record of the tree's shape — without this lane reading
-- anything back.
--
-- NO PAYLOAD HERE CARRIES A 'ContextRef', and none should.  It is a
-- capability, and the branch receipts belong to the RUNTIME
-- (@Event::SnapshotFrozen@ at a freeze, @Event::BranchInvocation@ at a
-- branched window's first turn, carrying the digest and the shared-prefix
-- byte count it re-derived itself).  A second, weaker claim minted beside
-- them by the harness would be a receipt about text the harness composed
-- rather than about the fork that actually happened.
-- ---------------------------------------------------------------------------

-- | THE OUTERMOST middleware: record the layer the driver will actually
-- descend through, after every policy has had its say.
--
-- Why this is a wrapper and not a line inside 'discover': `discover` runs
-- INNERMOST, so a layer it journaled could still be discarded by the fan-out
-- cap or reshaped by the gate.  Journaling there made kind @split@ name
-- children that never ran (a fan-out-capped node) and miss children the
-- operator added (a gated one) — and §10.1 promises that a @split@ entry's
-- child paths ARE the durable record of the tree's shape.  A resume fold
-- reading it would have rebuilt a tree the run never had.
--
-- So the two facts get two kinds, and neither is a summary of the other:
-- @proposed@ is what the WINDOW said (written by 'discover', the friction
-- log's raw material — a model's refused 7-branch layer is exactly what C6
-- wants to see), and @split@\/@finish@ is what the DRIVER did.  A capped node
-- honestly carries both: a @proposed@ naming seven branches and a @finish@
-- stamped @BudgetForced ForcedFanOut@.
journaled :: Coalg Companion NodeSeed -> Coalg Companion NodeSeed
journaled inner seed = do
  layer <- inner seed
  journalLayer seed layer
  pure layer

journalLayer :: NodeSeed -> ThoughtF NodeSeed -> Companion ()
journalLayer seed layer = case layer of
  Th.Finish d ->
    record "finish" key (object ["origin" .= renderOrigin d, "draft" .= d.draftText])
  _ ->
    record
      "split"
      key
      ( object
          [ "posture" .= posture
          , "focus" .= focus
          , "strategyProposed" .= show proposed
          , "strategyExecuted" .= show (executedStrategy proposed)
          , "branches" .= map branchEntry (layerBranches layer)
          ]
      )
  where
    key = renderPath seed.seedPath
    proposed = layerStrategy layer
    (posture, focus) = postureAndFocus layer
    branchEntry br =
      object
        [ "path" .= renderPath br.value.seedPath
        , "title" .= br.brief.title
        , "role" .= show br.brief.role
        ]

postureAndFocus :: ThoughtF a -> (Text, Text)
postureAndFocus layer = case layer of
  Th.Finish d -> ("Finish", renderOrigin d)
  Th.Explore f _ _ -> ("Explore", f)
  Th.Compare d _ _ -> ("Compare", d)
  Th.Challenge c _ _ -> ("Challenge", c)

-- ---------------------------------------------------------------------------
-- Prompts
--
-- Each window's prompt embeds its node's 'NodePath', which is what makes a
-- scripted needle-matched provider able to serve a whole deterministic tree
-- from a table of (path, reply) pairs.
--
-- A coalgebra prompt says NOTHING about what this node knows from above, and
-- that absence is the point: the window it is sent to was branched off the
-- parent's frozen context ('discover'), so the ancestry is the window's own
-- shared prefix rather than a summary of it rendered into a suffix.
-- ---------------------------------------------------------------------------

-- | Three complete, minimal, COMPILABLE @finalize@ examples (companion
-- review step 8f) — one per window type, teaching constructor nesting and
-- required-empty-list fields better than schema prose alone. Delimited with
-- a plain @--- EXAMPLE ---@ marker rather than a triple-backtick fence: a
-- markdown fence here would be the FIRST one in the assembled request,
-- ahead of the engine's own auto-rendered hole-card type shape, and
-- 'companion_recursive_slice.rs''s row-1 check reads exactly that FIRST
-- fenced block (@fenced_haskell@) — these examples must never compete with
-- it for that position.
exampleBlock :: Text -> Text
exampleBlock code =
  [fmt|--- EXAMPLE ---
{code}
--- END EXAMPLE ---|]

exampleProposeFinish :: Text
exampleProposeFinish =
  exampleBlock "finalize @LayerProposal (ProposeFinish { localAnswer = \"the answer, stated directly\" })"

exampleProposeSplit :: Text
exampleProposeSplit =
  exampleBlock
    "finalize @LayerProposal (ProposeSplit\n\
    \  { splitPosture = Explore\n\
    \  , splitFocus = \"what this node needs to settle\"\n\
    \  , splitStrategy = WantSequential\n\
    \  , splitBranches =\n\
    \      [ ProposedBranch { branchTitle = \"first angle\", branchRole = Primary, branchInstruction = \"work this angle\" } ]\n\
    \  })"

exampleFoldDecisionEmpty :: Text
exampleFoldDecisionEmpty =
  exampleBlock
    "finalize @FoldDecision (FoldDecision { foldSynthesis = \"what this node concludes\", foldTensions = [], foldEditsInOrder = [], foldProposed = [] })"

coalgebraPrompt :: Int -> NodeSeed -> Text
coalgebraPrompt maxFanOut seed =
  [fmt|NODE {renderPath seed.seedPath} — DISCOVER (depth {seed.seedDepth}, node allowance {seed.seedAllowance}).

{renderBrief seed.seedBrief}

Decide THIS LAYER and only this layer. You cannot describe a subtree: the
answer type has no recursive arm, by design. Either finish here, or name the
branches that should be worked next — each of them will be discovered the
same way you are being discovered now, and their results folded back to you.
At most {show maxFanOut} branches: a split naming more than that is treated
as a forced finish before any of those branches ever run.

Every record field is required — there are no optional fields on this type.

If this layer needs repository evidence or a code change to decide honestly,
delegate it to a coding subagent before you finalize:
`delegate (DelegateBrief {{ delegateLabel, delegateInstruction, delegateExpected }})
:: M (Either DelegateError DelegateResult)`. `delegateLabel` is a short slug;
`delegateInstruction` is the task in prose; `delegateExpected` says what a good
result looks like (may be blank). You get back `Left err` (render it with
`renderDelegateError`) or `Right r` with `delegateSummary r` and
`delegateCaveats r`. The subagent works in its own fresh worktree off the
current repository — there is no worktree or raw-subagent surface here, and
none is needed: bind the result, then finalize based on what it found.

If the OPERATOR's intent is genuinely ambiguous — the question underdetermines
a fork only they can steer — ask them: post the question with `note`, then
`askUser @OperatorSteering`; the reply's `steeringReply` field is their
answer. Ask ONLY when their answer would change what this window does; an
ask is a human interrupt, so otherwise decide, and record the assumption in
what you finalize.

Finalize a LayerProposal:
- `ProposeFinish {{ localAnswer }}` — this node answers locally. Say the
  answer, not a plan to produce it.
- `ProposeSplit {{ splitPosture, splitFocus, splitStrategy, splitBranches }}` —
  `splitPosture` is Explore (open the space), Compare (weigh named options),
  or Challenge (attack a claim); `splitFocus` is the focus, the decision, or
  the claim, per the posture; `splitStrategy` is what you would LIKE to run
  under. v1 executes all branches sequentially. Use WantSequential; the
  other constructors (WantConcurrent, WantPooled {{ pooledWidth = 2 }}) only
  record a preference, shown transformed in the receipt, never actually
  scheduled differently. `splitBranches` is a non-empty list of
  ProposedBranch {{ branchTitle, branchRole, branchInstruction }} with
  branchRole one of Primary, Alternative, Critic. A branch with a blank
  title or instruction, or a split with no branches, is treated as a failed
  window — not as a finish you chose.

Two complete examples:

{exampleProposeFinish}

{exampleProposeSplit}

Finalize: `finalize @LayerProposal (...)`|]

-- | Companion review step 4/8b/8c's wording for who a fold's selection
-- authorizes what for.
foldRoleWording :: Bool -> Text
foldRoleWording isRoot
  | isRoot =
      [fmt|This is the root fold. Your ordered selection is the final authorization. The runtime applies it once, in the order listed, against the turn-start draft; omitted edits are not persisted.|]
  | otherwise =
      [fmt|Selecting an edit here endorses it to this node's parent; it does not persist the edit. An available edit you omit is dropped from this route and cannot reach the root. The runtime evaluates your ordered selection against the turn-start draft only as a preview. Reassess independently — do not rubber-stamp a child's prior endorsement just because it reached you.|]

-- | Companion review step 4/8i's wording for what a fold may propose.
foldProposedWording :: Bool -> Text
foldProposedWording isRoot
  | isRoot =
      [fmt|This is the root fold: `foldProposed` must be empty — the root selects from the pool, it does not author new edits. Put anything else in `foldSynthesis`.|]
  | otherwise =
      [fmt|`foldProposed` is how THIS node contributes brand-new draft edits of its own: a list of AppendEdit {{{{ editIntent, editAppend }}}} or ReplaceOnce {{{{ editIntent, editNeedle, editReplacement }}}} values. editIntent must contain a specific reason for the change. Proposals with blank intent — or with no actual change — are rejected before they are advertised to a parent. A node's own `foldProposed` artifacts are never selectable at its OWN fold — only its PARENT's fold, one level up, can select them.|]

-- | @pool@\/@poolRenders@ are every immediate child's own advertised
-- artifacts — this node's own selectable pool (PRD 21 lane C4), grouped by
-- IMMEDIATE CHILD ("who endorsed this to me", 'renderPoolBlock') with each
-- artifact's own full content shown, never just its intent (companion
-- review DEFECT 1). @mergeNotes@ is PRD 21 lane C5's merge fold, already
-- run by the time this prompt is built ('foldAt' calls 'mergeFold' before
-- 'foldWindow') — this is the "folded as data for the algebra to decide"
-- half of decision 6: the model reads what happened and may say so in
-- 'foldTensions'\/'foldSynthesis', but nothing here presents it as a form
-- or forces a response. Rendered ABOVE the branch block on purpose:
-- 'companion_recursive_slice.rs''s @branch_summary@ reads a branch's own
-- text up to the NEXT "--- branch " or the literal "\\n\\nFold this
-- realized layer" marker, so nothing may sit between the LAST branch's
-- block and that marker without silently widening what a sibling's "own"
-- summary is read to contain. @mergeSection@ carries its own leading blank
-- line and is the empty string when @mergeNotes@ is @[]@, so a node with
-- no worktree content renders BYTE-IDENTICAL to before that lane existed.
algebraPrompt ::
  NodePath ->
  ThoughtF a ->
  [Th.Branch NodeAnswer] ->
  [Th.Artifact Text] ->
  [(Th.ArtifactId, Text)] ->
  Text ->
  Bool ->
  [Text] ->
  Text
algebraPrompt path layer kids _pool poolRenders draft isRoot mergeNotes =
  [fmt|NODE {renderPath path} — FOLD ({posture}: {focus}).

{draftBlock}

{poolBlock}{mergeSection}

{childBlock}

Fold this realized layer into one answer. The branches are in DECLARED order,
never completion order, and a branch that failed is an ordinary value in the
list — say what it cost you rather than pretending it did not happen.

{roleWording}

Every record field is required. For a narrative-only fold, set
foldEditsInOrder = [] and foldProposed = []. Use foldEditsInOrder only for
available edit IDs, listed once each in exact application order.

Finalize a FoldDecision {{ foldSynthesis, foldTensions, foldEditsInOrder, foldProposed }}:
`foldSynthesis` is this node's answer as prose, written to be read on its own;
`foldTensions` are the disagreements the branches did NOT resolve, one per
entry, kept rather than averaged away. `foldEditsInOrder` names artifact ids
from the pool above, each listed once, in the order to apply them.
{proposedWording}

Example:

{exampleFoldDecisionEmpty}

Finalize: `finalize @FoldDecision (...)`|]
  where
    (posture, focus) = postureAndFocus layer
    -- (d) the exact turn-start draft, as a clearly delimited multiline
    -- block — a fold authoring/approving edits must see its actual target.
    draftBlock =
      [fmt|The companion's working draft AS OF TURN START — what any selected
or proposed edit actually runs against:
--- BEGIN DRAFT ---
{draft}
--- END DRAFT ---|]
    -- (2) DEFECT fix: a childless fold's prompt must show the Finish
    -- layer's own draftText, not just say "you are folding a local finish".
    childBlock = case layer of
      Th.Finish d -> localFinishBlock d.draftText
      _ -> T.intercalate "\n\n" (map childSummary kids)
    poolBlock = renderPoolBlock kids poolRenders
    mergeSection = case mergeNotes of
      [] -> "" :: Text
      ns -> "\nWorktree merges into this node:\n" <> T.intercalate "\n" (map ("- " <>) ns)
    roleWording = foldRoleWording isRoot
    proposedWording = foldProposedWording isRoot

-- | Companion review step 2's fix: the local result a childless fold is
-- actually folding, in a delimited block — never just a description that
-- one exists.
localFinishBlock :: Text -> Text
localFinishBlock t =
  [fmt|This node has no branches: it finished locally. Its own local result:
--- BEGIN LOCAL FINISH ---
{t}
--- END LOCAL FINISH ---|]

-- | The pool, GROUPED BY IMMEDIATE CHILD (companion review DEFECT 1): who
-- endorsed each artifact to this fold, with its own path-namespaced id
-- shown under it (endorsement propagation keeps ids unchanged end to end,
-- so the id alone already names where an artifact originated). Evidence
-- artifacts are a SEPARATE, explicitly non-selectable section — before this
-- fix they were listed alongside edits, selectable-looking, then silently
-- dropped by 'Th.resolveSelection' (an 'Th.EvidenceArtifact' can never
-- become an 'Th.EditPlan').
renderPoolBlock :: [Th.Branch NodeAnswer] -> [(Th.ArtifactId, Text)] -> Text
renderPoolBlock kids poolRenders
  | null editGroups = "No artifacts are available to select at this fold." <> evidenceBlock
  | otherwise =
      "Available artifacts — select by id, each at most once:\n"
        <> T.intercalate "\n\n" (map renderEditGroup editGroups)
        <> evidenceBlock
  where
    childEdits kid = filter isEditArtifact kid.value.answerArtifacts
    childEvidence kid = filter (not . isEditArtifact) kid.value.answerArtifacts
    editGroups = [(kid, as) | kid <- kids, let as = childEdits kid, not (null as)]
    evidenceGroups = [(kid, es) | kid <- kids, let es = childEvidence kid, not (null es)]
    renderEditGroup (kid, as) =
      [fmt|From {kid.brief.title} ({renderPath kid.value.answerPath}):
{T.intercalate "\n" (map (renderPoolArtifact poolRenders) as)}|]
    evidenceBlock = case evidenceGroups of
      [] -> "" :: Text
      _ ->
        "\n\nEvidence — NOT selectable, informational only:\n"
          <> T.intercalate "\n\n" (map renderEvidenceGroup evidenceGroups)
    renderEvidenceGroup (kid, es) =
      [fmt|From {kid.brief.title} ({renderPath kid.value.answerPath}):
{T.intercalate "\n" (map renderEvidence es)}|]

-- | One selectable edit artifact, with its FULL content — the exact
-- appended text (an 'AppendEdit'), or the needle AND replacement both (a
-- 'ReplaceOnce') — rendered from 'poolRenders', never just the declared
-- intent (companion review DEFECT 1).
renderPoolArtifact :: [(Th.ArtifactId, Text)] -> Th.Artifact Text -> Text
renderPoolArtifact renders a = case a of
  Th.EditArtifact aid (Th.EditIntent intent) _ ->
    [fmt|- {artifactIdText aid}: {intent}
{content}|]
    where
      content = case lookup aid renders of
        Just r -> r
        Nothing -> "  <no content preview available>" :: Text
  Th.EvidenceArtifact {} -> "" -- unreachable: 'renderPoolBlock' routes evidence elsewhere

renderEvidence :: Th.Artifact Text -> Text
renderEvidence a = case a of
  Th.EvidenceArtifact aid (Th.Evidence e) -> [fmt|- {artifactIdText aid}: {e}|]
  Th.EditArtifact {} -> "" -- unreachable: 'renderPoolBlock' routes edits elsewhere

-- | (e) each branch's own ASSIGNMENT (the instruction it was given),
-- alongside what it answered — before this fix a fold saw only answers,
-- never the instructions they were meant to satisfy.
childSummary :: Th.Branch NodeAnswer -> Text
childSummary (Th.Branch b a) =
  [fmt|--- branch {renderPath a.answerPath}: {b.title} ({show b.role}) [{a.answerPosture}]
instruction: {b.instruction}
{a.answerSynthesis}{tensions}|]
  where
    tensions = case a.answerTensions of
      [] -> "" :: Text
      ts -> "\nunresolved: " <> T.intercalate "; " ts

-- | The corrective-retry window's own prompt (companion review step 6c):
-- the exact validation-failure preamble, quoting which ids were
-- unavailable and which were duplicated, the prior decision echoed
-- verbatim (its own derived 'Show'), and then the SAME authoritative
-- prompt ('algebraPrompt') the model saw the first time — pool, branches,
-- draft, and every wording rule unchanged, so the retry is a genuine
-- second attempt at the same question rather than a narrower one.
correctiveRetryPrompt ::
  NodePath ->
  ThoughtF a ->
  [Th.Branch NodeAnswer] ->
  [Th.Artifact Text] ->
  [(Th.ArtifactId, Text)] ->
  Text ->
  Bool ->
  [Text] ->
  [Text] ->
  [Text] ->
  FoldDecision ->
  Text
correctiveRetryPrompt path layer kids pool poolRenders draft isRoot mergeNotes unavailableIds duplicateIds priorFd =
  [fmt|Your edit selection did not validate. These IDs are unavailable: {renderIdList unavailableIds}. These IDs are duplicated: {renderIdList duplicateIds}. Choose only unique IDs from the authoritative list below, or return an empty list.

Your previous decision, for reference:
{show priorFd}

{algebraPrompt path layer kids pool poolRenders draft isRoot mergeNotes}|]
  where
    renderIdList ids = case ids of
      [] -> "(none)" :: Text
      _ -> T.intercalate ", " ids
