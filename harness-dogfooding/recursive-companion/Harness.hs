{-# LANGUAGE DataKinds #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | The recursive companion (PRD 21 lane C3): one root turn in which a
-- coalgebra window finalizes a 'ThoughtF' layer (or a local finish), each
-- branch descends recursively from its parent's FROZEN context, an algebra
-- window folds typed results in declared branch order, and the operator gets
-- a folded answer with the tree inspectable but not primary.
--
-- __The recursion lives HERE, in the authored outer loop, and that is
-- forced.__  @Harness::new@ builds a window's @child_cfg@ as
-- @fork_child_decls(&cfg.decls)@ — this node's row MINUS the fork-spawning
-- effects — so a fork\/fanout child compiles against a row with no @Fork@ in
-- it and structurally cannot produce grandchildren.  Depth would stop at two.
-- Recursion therefore lives at the one place @Fork@ is never removed: the
-- authored loop.
--
-- __The two seams.__  Cognition enters at exactly two typed windows —
-- 'discover' (ONE @runLLMTurnBranch \@LayerProposal@ per node, forked off the
-- parent's frozen post-coalgebra context) and 'foldNode' (ONE
-- @runLLMTurnFork \@FoldProposal@ per node).  Everything else in this
-- file is compiled coordination and costs zero tokens.  The algebra runs at
-- EVERY node (PRD 21 locked decision 8): a leaf's algebra sees a realized
-- layer with no children, and that is the uniform place a fold becomes
-- durable.
--
-- __Assumed row.__  @Companion@ is an alias for @M@, and this file needs
-- @RunLLMTurn@, @AskUser@, @Console@ and @Journal@ — a subset of the driver's
-- widened outer session (@selfharness::driver::outer_decls@).  It declares no
-- new effect and asks for no row widening.
-- @tidepool-harness\/tests\/dogfood_harness_typecheck.rs@ compiles it against
-- that full row.
module Harness
  ( -- * The locked entry points
    State (..)
  , Config (..)
  , RunSummary (..)
  , initialState
  , render
  , loop

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
  , FoldProposal (..)

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
  ) where

import qualified Data.List.NonEmpty as NE
import qualified Data.Text as T
import HarnessTypes
import Tidepool.Aeson (Value, object, toJSON, (.=))
import Tidepool.Effects
  ( ContextRef
  , InvocationExit
  , freezeContext
  , renderInvocationExit
  , runLLMTurnBranch
  , runLLMTurnFork
  , say
  )
import Tidepool.Form (askUser)
import Tidepool.Harness (Harness)
import Tidepool.Journal (record)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import Tidepool.Swarm (cyclesToInt, mkCycles, splitAllowance)
import Tidepool.Thought (Coalg, Strategy, ThoughtF, depthCapped, fanOutCapped, thoughtHylo)
import qualified Tidepool.Thought as Th

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
loop st = do
  record "turn" rootKey (object ["root" .= st.question, "config" .= toJSON cfg])
  rootRef <- freezeContext
  f <- thoughtHylo (foldNode cfg) coalg (rootSeed st rootRef)
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
  pure st {turnCount = st.turnCount + 1, lastRun = Just (summarize answer)}
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
layerWindow :: ContextRef -> Text -> Companion (Either InvocationExit (LayerProposal, ContextRef))
layerWindow ref prompt = runLLMTurnBranch @LayerProposal ref prompt

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
foldWindow :: Text -> Companion (Either InvocationExit FoldProposal)
foldWindow prompt = runLLMTurnFork @FoldProposal prompt

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
discover _cfg seed = do
  outcome <- layerWindow seed.seedRef (coalgebraPrompt seed)
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
  Right (ProposeFinish {finishDraft = t}, _) -> object ["finish" .= t]
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
  ProposeFinish {finishDraft = t} -> Th.Finish (Th.Draft t Th.ModelFinished seed.seedDepth)
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
foldNode :: Config -> ThoughtF Folded -> Companion Folded
foldNode _cfg layer = pure (foldAt layer)

foldAt :: ThoughtF Folded -> Folded
foldAt layer path = do
  realized <- traverseLayer (layerStrategy layer) (applyChild path) (indexLayer layer)
  let kids = layerBranches realized
      kidAnswers = map (.value) kids
  outcome <- foldWindow (algebraPrompt path layer kids)
  -- A FOLD window that exits is this node's OWN failure, and it must not be
  -- its subtree's.  The children below it already ran and already folded;
  -- discarding their answers, their tree lines, or their accounting here
  -- would erase completed sibling work one level up — the same erasure
  -- decision 6 forbids at a branch position, just reached from the algebra
  -- side.  So the exit replaces only what this node itself was going to
  -- contribute (its synthesis and tensions), and everything the children
  -- earned rolls up untouched.
  let (synthesis, tensions, foldBadges, foldFailed) = case outcome of
        Right p -> (p.foldSynthesis, p.foldTensions, [], 0)
        Left e ->
          ( [fmt|<this node's fold window exited: {renderInvocationExit e}> — its {show (length kids)} branch result(s) are below, unfolded|]
          , []
          , ["fold failed"]
          , 1
          )
  record
    "fold"
    key
    ( object
        [ "synthesis" .= synthesis
        , "tensions" .= tensions
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
  pure
    NodeAnswer
      { answerPath = path
      , answerPosture = layerPosture layer
      , answerSynthesis = synthesis
      , answerTensions = tensions
      , answerBadges = strategyBadges (layerStrategy layer) <> originBadges layer <> foldBadges
      , answerTree = concatMap childLines kids
      , answerNodes = 1 + sum (map (.answerNodes) kidAnswers)
      , answerWindows = selfWindows layer + sum (map (.answerWindows) kidAnswers)
      , answerForced = selfForced layer + sum (map (.answerForced) kidAnswers)
      , answerFailed = selfFailed layer + foldFailed + sum (map (.answerFailed) kidAnswers)
      }
  where
    key = renderPath path
    -- The ONE place a child's identity is computed on the fold side, and it
    -- is 'childPath' — the same function the coalgebra used to build the
    -- child's seed, over the same branch list in the same order carrying the
    -- same brief title.  The two cannot disagree.
    applyChild p (Th.Branch b (i, f)) = f (childPath p i b.title)
    childLines (Th.Branch b a) = subtreeLines b.title a

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

coalgebraPrompt :: NodeSeed -> Text
coalgebraPrompt seed =
  [fmt|NODE {renderPath seed.seedPath} — DISCOVER (depth {seed.seedDepth}, node allowance {seed.seedAllowance}).

{renderBrief seed.seedBrief}

Decide THIS LAYER and only this layer. You cannot describe a subtree: the
answer type has no recursive arm, by design. Either finish here, or name the
branches that should be worked next — each of them will be discovered the
same way you are being discovered now, and their results folded back to you.

Finalize a LayerProposal:
- `ProposeFinish {{ finishDraft }}` — this node answers locally. Say the
  answer, not a plan to produce it.
- `ProposeSplit {{ splitPosture, splitFocus, splitStrategy, splitBranches }}` —
  `splitPosture` is Explore (open the space), Compare (weigh named options),
  or Challenge (attack a claim); `splitFocus` is the focus, the decision, or
  the claim, per the posture; `splitStrategy` is what you would LIKE
  (WantSequential / WantConcurrent / WantPooled), recorded and shown
  transformed if the driver runs it differently; `splitBranches` is a
  non-empty list of ProposedBranch {{ branchTitle, branchRole, branchInstruction }}
  with branchRole one of Primary, Alternative, Critic. A branch with a blank
  title or instruction, or a split with no branches, is treated as a failed
  window — not as a finish you chose.

Finalize: `finalize @LayerProposal (...)`|]

algebraPrompt :: NodePath -> ThoughtF a -> [Th.Branch NodeAnswer] -> Text
algebraPrompt path layer kids =
  [fmt|NODE {renderPath path} — FOLD ({posture}: {focus}).

{childBlock}

Fold this realized layer into one answer. The branches are in DECLARED order,
never completion order, and a branch that failed is an ordinary value in the
list — say what it cost you rather than pretending it did not happen.

Finalize a FoldProposal {{ foldSynthesis, foldTensions }}: `foldSynthesis` is
this node's answer as prose, written to be read on its own; `foldTensions` are
the disagreements the branches did NOT resolve, one per entry, kept rather
than averaged away.

Finalize: `finalize @FoldProposal (...)`|]
  where
    (posture, focus) = postureAndFocus layer
    childBlock = case kids of
      [] -> "This node has no branches: you are folding a local finish." :: Text
      _ -> T.intercalate "\n\n" (map childSummary kids)

childSummary :: Th.Branch NodeAnswer -> Text
childSummary (Th.Branch b a) =
  [fmt|--- branch {renderPath a.answerPath}: {b.title} ({show b.role}) [{a.answerPosture}]
{a.answerSynthesis}{tensions}|]
  where
    tensions = case a.answerTensions of
      [] -> "" :: Text
      ts -> "\nunresolved: " <> T.intercalate "; " ts
