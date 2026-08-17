{-# LANGUAGE DataKinds #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | The recursive companion (PRD 21 lane C3): one root turn in which a
-- coalgebra window finalizes a 'ThoughtF' layer (or a local finish), each
-- branch descends recursively from inherited context, an algebra window folds
-- typed results in declared branch order, and the operator gets a folded
-- answer with the tree inspectable but not primary.
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
-- 'discover' (ONE @runLLMTurnFork \@LayerProposal@ per node) and 'foldNode'
-- (ONE @runLLMTurnFork \@FoldProposal@ per node).  Everything else in this
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
  , InheritedContext (..)
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
  , childAllowance
  ) where

import qualified Data.Text as T
import HarnessTypes
import Tidepool.Aeson (object, toJSON, (.=))
import Tidepool.Effects (runLLMTurnFork, say)
import Tidepool.Form (askUser)
import Tidepool.Harness (Harness)
import Tidepool.Journal (record)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
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
loop :: State -> Companion State
loop st = do
  record "turn" rootKey (object ["root" .= st.question, "config" .= toJSON cfg])
  f <- thoughtHylo (foldNode cfg) coalg (rootSeed st)
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
      gatedLayer
        cfg
        ( fanOutCapped
            seedDepth
            cfg.maxFanOut
            (depthCapped seedDepth cfg.maxDepth (allowanceCapped (discover cfg)))
        )

-- | The root's own seed.  Its brief IS the operator's question, so the root
-- window is asked the same shape of thing every descendant is.
rootSeed :: State -> NodeSeed
rootSeed st =
  NodeSeed
    { seedPath = NodePath []
    , seedBrief = Th.ForkBrief "root" Th.Primary st.question
    , seedDepth = 0
    , seedAllowance = st.config.maxNodes
    , seedContext = InheritedContext {inheritedAncestry = [], inheritedDecision = ""}
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
-- Gap 3 (a window's abnormal exit aborting its siblings) is closed in a
-- sibling lane: @runLLMTurnFork@ becomes per-child-typed,
-- @M (Either InvocationExit T)@, so a branch's round exhaustion or
-- non-finalization folds as a failure AT ITS BRANCH POSITION instead of
-- erasing every sibling result.  Both invocations are funnelled through the
-- two functions below so that swap is one edit each:
--
-- >   Right v   -> pure v
-- >   Left exit -> ... Finish (Draft (renderInvocationExit exit)
-- >                            (InvocationFailed (NodeFailure ...)) depth)
--
-- TWO functions rather than one @runWindow :: Text -> Companion a@, and the
-- reason is mechanical: extract's typed-yield site pass rejects a
-- @runLLMTurnFork \@a@ call at a bare type VARIABLE ("polymorphic runLLMTurn
-- site"), so a polymorphic wrapper cannot exist.  They stay adjacent, and
-- they are the only two call sites.
--
-- @runLLMTurnFork@, never plain @runLLMTurn@: the plain form lands on the
-- driver's ONE reused per-loop answerer node, which accumulates every hole's
-- exchange into a single flat context — that would put every sibling's output
-- into every later node's window, exactly what locked decision 2 forbids.
-- The fork form mints a fresh answerer node per window.
-- ---------------------------------------------------------------------------

layerWindow :: Text -> Companion LayerProposal
layerWindow prompt = runLLMTurnFork @LayerProposal prompt

foldWindow :: Text -> Companion FoldProposal
foldWindow prompt = runLLMTurnFork @FoldProposal prompt

-- ---------------------------------------------------------------------------
-- The coalgebra — how to split
-- ---------------------------------------------------------------------------

-- | Unfold one node: ONE window, then the pure conversion, then the journal.
-- Every policy that could refuse this is middleware wrapped around it at the
-- 'loop' call site, so what is left here is only what splitting MEANS.
discover :: Config -> NodeSeed -> Companion (ThoughtF NodeSeed)
discover _cfg seed = do
  proposal <- layerWindow (coalgebraPrompt seed)
  let layer = layerFromProposal seed proposal
  journalLayer seed layer
  pure layer

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
      case applyGate approval layer of
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
  proposal <- foldWindow (algebraPrompt path layer kids)
  record
    "fold"
    key
    ( object
        [ "synthesis" .= proposal.foldSynthesis
        , "tensions" .= proposal.foldTensions
        , "children" .= map (renderPath . (.answerPath)) kidAnswers
        , "depth" .= pathDepth path
        ]
    )
  case failureReason layer of
    Nothing -> pure ()
    Just why -> record "failed" key (object ["reason" .= why])
  pure
    NodeAnswer
      { answerPath = path
      , answerPosture = layerPosture layer
      , answerSynthesis = proposal.foldSynthesis
      , answerTensions = proposal.foldTensions
      , answerBadges = strategyBadges (layerStrategy layer) <> originBadges layer
      , answerTree = concatMap childLines kids
      , answerNodes = 1 + sum (map (.answerNodes) kidAnswers)
      , answerWindows = selfWindows layer + sum (map (.answerWindows) kidAnswers)
      , answerForced = selfForced layer + sum (map (.answerForced) kidAnswers)
      , answerFailed = selfFailed layer + sum (map (.answerFailed) kidAnswers)
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
traverseLayer _proposed visit layer = do
  visited <- traverse step (layerBranches layer)
  pure (rebuildLayer layer visited)
  where
    step br = do
      v <- visit br
      pure (Th.Branch br.brief v)

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
-- ---------------------------------------------------------------------------

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
          , -- NOT a digest and NOT a cache claim: the exact byte length of the
            -- prompt-rendered inheritance this node's children receive.  There
            -- is no snapshot to name (see 'InheritedContext').
            "inheritedBytes" .= utf8Bytes (renderInherited seed.seedContext)
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
-- ---------------------------------------------------------------------------

coalgebraPrompt :: NodeSeed -> Text
coalgebraPrompt seed =
  [fmt|NODE {renderPath seed.seedPath} — DISCOVER (depth {seed.seedDepth}, node allowance {seed.seedAllowance}).

{renderInherited seed.seedContext}

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
