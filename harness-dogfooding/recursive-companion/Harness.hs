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
-- Cognition enters at exactly two windows, both branched — 'discoverWith'
-- (off the parent's frozen post-coalgebra context, one bulk call per
-- sibling group via 'bulkLayerWindow') and 'foldAt' (off THIS node's own
-- post-coalgebra context, via 'foldWindow') — every other function here is
-- compiled coordination.
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
-- window: node windows ('bulkLayerWindow'\/'foldWindow') compile against the
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
  , NodeSeed (..)
  , NodePath (..)
  , NodeAnswer (..)

    -- * What a window may finalize
  , LayerProposal (..)
  , ProposedBranch (..)
  , Posture (..)
  , BranchRoleWire (..)
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

import qualified Data.List as L
import Data.List.NonEmpty (NonEmpty ((:|)))
import qualified Data.List.NonEmpty as NE
import Data.Maybe (catMaybes, fromMaybe)
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
  , runLLMTurnBranch
  , runLLMTurnBranchFanout
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
import Tidepool.Thought (Coalg, ThoughtF, depthCapped, fanOutCapped)
import qualified Tidepool.Thought as Th
import Tidepool.Worktree

-- | The orchestration monad.  @Harness@ is @M@ under a friendlier name; the
-- row it resolves to is the driver's outer session.
type Companion = Harness

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
    -- ('discoverWith') — PRD 21 locked decision 2, made real: a child forks
    -- its parent's frozen post-coalgebra window rather than reading a
    -- rendered summary of it, so the shared prefix is the ancestor's actual
    -- transcript and only the divergent suffix is new.
    --
    -- __It is a CAPABILITY, threaded as a value only.__  It never enters
    -- 'State' (which is checkpointed JSON), is never interpolated into a
    -- prompt, and is never reconstructed from text — possession is
    -- permission, and a ref only ever comes from @freezeContext@ or from a
    -- @runLLMTurnBranchFanout@ sibling's own return.  @ContextRef@ has
    -- 'Show' and 'Eq', so the seed's deriving clause is unaffected — but
    -- 'Show' is not a channel:
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
-- 'discoverWith' re-stamps the seed with the ref its OWN coalgebra window
-- froze before building this layer, so what a child forks is its parent's
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

-- | One resident turn discovers a tree and folds it — as ONE recursive
-- walk ('walkGroup'\/'walkNode'), not two passes.  A node's own fold is now
-- a direct continuation of its own DISCOVER turn ('foldAt', branched off
-- the SAME ref its children branch from), run the moment its own subtree
-- finishes — never a fresh fork deferred to a second, whole-tree pass.
-- Folds therefore INTERLEAVE with descent: a node folds while a cousin
-- subtree may still be discovering (operator decision, 2026-08-22).
--
-- POLICY IS MIDDLEWARE, composed by ordinary function application over
-- @Tidepool.Thought@'s own @Coalg -> Coalg@ combinators, applied PER SEED
-- inside 'discoverGroup' — see that function's doc for exactly where each
-- cap fires relative to the ONE bulk window call a sibling group shares
-- (operator decision: sibling branch windows are ALWAYS driven concurrently,
-- transparently — scheduling is never a model-visible choice).
--
-- ORDER is still declared order, never completion order: discoveries happen
-- in declared branch order (a sibling group's bulk window answers in the
-- same order it was asked, exactly like 'ThoughtF''s own derived
-- @Traversable@) and 'walkGroup' recurses\/folds each sibling in that same
-- order too.
--
-- THE ROOT'S REF IS MINTED HERE, and that is what makes 'walkGroup' uniform.
-- @freezeContext@ freezes the CALLING window — this loop's own accumulated
-- context, which is precisely what the root's coalgebra should fork from —
-- so the root enters the walk holding a ref exactly like every descendant
-- does (as a sibling group of exactly one), and 'discoverGroup' has no root
-- special case to get wrong.
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
          say [fmt|Question seeded: {q}
Starting the first turn.|]
          -- RECURSE, don't return: seeding IS the operator's "go" — ending
          -- the cycle here would park them on a between-turns gate that
          -- asks them to confirm the thing they just did (dogfood finding,
          -- 2026-08-20). The price is that the seed only checkpoints once
          -- turn 1 completes, so a mid-turn crash re-asks the question —
          -- one cheap re-type against one pointless click per fresh run.
          loop st {question = q}
loop st = do
  record "turn" rootKey (object ["root" .= st.question, "config" .= toJSON cfg])
  rootRef <- freezeContext
  answer <- NE.head <$> walkGroup cfg (rootSeed st rootRef :| [])
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
      }
  where
    cfg = st.config
    rootKey = renderPath (NodePath [])

-- | The honest opt-out ('Tidepool.Resume' module doc): this harness's
-- 'record' calls exist for the durable transcript, not to replay prior
-- windows on a resumed boot — a rerun re-derives 'lastRun' from
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
-- verb: both @runLLMTurnBranchFanout \@T@ and @runLLMTurnBranch \@T@ answer
-- an @Either InvocationExit _@, so the DRIVER no longer fails the whole
-- outer turn when one branch's window exhausts its rounds — the exit
-- arrives as data at that branch's position
-- (plans/self-iterating-harness/21-c3-exit-verb.md).
--
-- Both seams pass the @Either@ through UNWRAPPED, and their two callers fold
-- it — because the two exits mean different things and the difference is the
-- whole point:
--
-- * a COALGEBRA exit means this node decided no layer, so 'discoverWith'
--   makes it a leaf whose @FinishOrigin@ is @InvocationFailed@.  The node's own algebra
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
-- Neither is plain @runLLMTurn@: the plain form lands on the driver's ONE
-- reused per-loop answerer node, which accumulates every hole's exchange into
-- a single flat context — that would put every sibling's output into every
-- later node's window, exactly what locked decision 2 forbids.  Both forms
-- below mint a fresh answerer node per window.
-- ---------------------------------------------------------------------------

-- | The COALGEBRA's window, BULK-BRANCHED off ONE shared @ref@ — every seed
-- in a sibling group forks off the SAME frozen context that ref names,
-- never an empty root, in ONE 'runLLMTurnBranchFanout' call (operator
-- decision: sibling branch windows are ALWAYS driven concurrently,
-- transparently — scheduling is never a model-visible choice, so there is
-- no separate sequential path left to fall back to). Each sibling's own
-- answer AND its own post-finalize ref come back at ITS OWN position, in
-- declared order — the ref is what lets the next layer down branch off
-- THAT node ('childSeed').
--
-- Every child is labeled with its own rendered 'NodePath' (PRD 21 C5 GUI
-- lane) — @root@ for the root window, @root\/1-x@ etc. for a descendant —
-- so the per-node operator GUI can register and route each window's own
-- asks/notes to its own panel instead of the default one. The label rides
-- the wire structurally, never parsed back out of the prompt.
bulkLayerWindow ::
  Config ->
  ContextRef ->
  NonEmpty NodeSeed ->
  Companion (NonEmpty (Either InvocationExit (LayerProposal, ContextRef)))
bulkLayerWindow cfg ref seeds =
  NE.fromList
    <$> runLLMTurnBranchFanout @LayerProposal
      ref
      (NE.toList (fmap (\s -> (renderPath s.seedPath, coalgebraPrompt cfg s)) seeds))

-- | The ALGEBRA's window, BRANCHED off THIS node's own ref — the fold is a
-- continuation of the node's own conversation (its own DISCOVER turn, its
-- own ProposeSplit), never a fresh fork: the unified recursive walk
-- ('walkNode') keeps a node's own post-coalgebra ref in lexical scope right
-- up to the point its own fold runs.
--
-- PRD 21 still says the algebra's model window gets a RENDERED view of the
-- realized layer, never the live value ('algebraPrompt' builds it) —
-- mounting the live value into the window instead is the escalation PRD
-- open question 3 gates, explicitly out of v1.  Branching changes WHERE the
-- fold's context comes from, not WHAT it is shown.
foldWindow :: ContextRef -> Text -> Companion (Either InvocationExit (FoldDecision, ContextRef))
foldWindow ref prompt = runLLMTurnBranch @FoldDecision ref prompt

-- ---------------------------------------------------------------------------
-- The coalgebra — how to split
-- ---------------------------------------------------------------------------

-- | The pure conversion, then the journal — factored apart from the window
-- call itself so 'discoverGroup' can run it uniformly over BOTH a live
-- bulk-window outcome and a budget-capped seed that never got one (see that
-- function's doc).
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
discoverWith :: NodeSeed -> Either InvocationExit (LayerProposal, ContextRef) -> Companion (ThoughtF NodeSeed)
discoverWith seed outcome = do
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
      , "branches" .= map (.branchTitle) p.splitBranches
      ]

-- | Discover ONE sibling group's own next layer: every seed is windowed
-- with a SINGLE bulk 'runLLMTurnBranchFanout' call ('bulkLayerWindow') —
-- operator decision: sibling branch windows are ALWAYS driven concurrently,
-- transparently, because each sibling's window is independent of its
-- neighbours' — never one at a time the way a per-seed 'Coalg' would force.
--
-- A seed a budget already refuses ('preCapped') never enters the bulk call
-- at all — spending a real window on a seed 'depthCapped'\/'allowanceCapped'
-- would refuse anyway is exactly the waste those caps exist to prevent, so
-- 'bulkLayerWindow' only ever windows the group's UN-capped members, and
-- 'unresolvedOutcome' below stands in for the rest — a value 'depthCapped'\/
-- 'allowanceCapped' are GUARANTEED to short-circuit before ever forcing,
-- since they run the SAME two checks 'preCapped' already made.
--
-- Every OTHER policy — fan-out, the operator gate, and the journal — still
-- runs per seed, through the SAME middleware composition 'loop' used to
-- build once for the whole tree: @journaled . gatedLayer cfg . fanOutCapped
-- ... . depthCapped ... . allowanceCapped@, applied here per seed so a
-- batched window call changes nothing about what each seed's own result is
-- allowed to become.
--
-- ALSO returns, per seed, the 'ContextRef' this node's OWN coalgebra window
-- actually froze — 'Nothing' for a seed that never got a real window
-- (pre-capped, or an abnormal exit). A node's fold is a 'runLLMTurnBranch'
-- continuation of its own DISCOVER turn ('foldWindow'), so that ref has to
-- survive past this middleware chain to the fold call site — 'walkNode'
-- reads it back off here, captured BEFORE the chain runs (so it is
-- unaffected by whatever the gate does to the layer afterward — see
-- 'applyGate''s own doc on which seed IT rebuilds branches against;
-- untouched, and irrelevant to this capture).
discoverGroup :: Config -> NonEmpty NodeSeed -> Companion (NonEmpty (ThoughtF NodeSeed, Maybe ContextRef))
discoverGroup cfg seeds = do
  outcomes <- bulkDiscoverOutcomes cfg seeds
  traverse (\(seed, outcome) -> perSeed seed outcome) outcomes
  where
    perSeed seed outcome = do
      layer <-
        journaled
          ( gatedLayer
              cfg
              ( fanOutCapped
                  seedDepth
                  cfg.maxFanOut
                  (depthCapped seedDepth cfg.maxDepth (allowanceCapped cfg.maxNodes (`discoverWith` outcome)))
              )
          )
          seed
      pure (layer, myRefOf seed outcome)
    -- Never forces @outcome@ for a pre-capped seed — the SAME guarantee
    -- 'unresolvedOutcome' below rests on, re-checked here because this is a
    -- second, independent site that touches the same value.
    myRefOf seed outcome
      | preCapped cfg seed = Nothing
      | otherwise = case outcome of
          Right (_, ref) -> Just ref
          Left _ -> Nothing

-- ---------------------------------------------------------------------------
-- The unified recursive walk: bulk-discover a sibling group
-- ('discoverGroup'), then per node recurse into its OWN children — a NEW
-- sibling group — and fold IMMEDIATELY, in LEXICAL SCOPE: the fold is a
-- continuation of the node's own conversation (its own DISCOVER turn, its
-- own ProposeSplit), branched off the SAME ref its own children branch
-- from, never a fresh fork off an empty root.
--
-- Folds therefore INTERLEAVE with descent: a node folds the moment its own
-- subtree completes, while a cousin subtree may still be discovering —
-- intended, not incidental (operator decision: supports future
-- multi-round forking). Concretely, siblings recurse in DECLARED order
-- ('traverse' over the sibling group), so one sibling's ENTIRE subtree —
-- every descendant's own discover-then-fold, then that sibling's own fold —
-- completes before the NEXT sibling's own children are even discovered.
-- The assembled RESULT does not depend on this: every sibling's own
-- 'NodeAnswer' is still assembled in declared order regardless ('walkGroup'
-- returns a 'NonEmpty' in the SAME order it was given), so nothing
-- downstream can observe which subtree actually finished first.
-- ---------------------------------------------------------------------------

-- | Discover, recurse and fold one whole sibling group, in declared order.
walkGroup :: Config -> NonEmpty NodeSeed -> Companion (NonEmpty NodeAnswer)
walkGroup cfg seeds = do
  discovered <- discoverGroup cfg seeds
  traverse (uncurry (walkNode cfg)) (NE.zip seeds discovered)

-- | One node: recurse into its own children (if any) as a new sibling
-- group, then fold — off this node's own post-coalgebra ref when its
-- coalgebra actually minted one, else its own PRE-coalgebra ref (@seedRef@,
-- the ref this node's OWN window branched from). That fallback is reachable
-- only for a CHILDLESS ROOT whose own coalgebra window never finalized
-- (an abnormal exit): 'foldAt''s own @mechanicalLeaf@ rule keeps every
-- OTHER childless node from ever reaching a fold call at all, so this is
-- root's one genuine "no special case" — it folds off whatever its own
-- best available ref is, exactly as every other node does.
walkNode :: Config -> NodeSeed -> (ThoughtF NodeSeed, Maybe ContextRef) -> Companion NodeAnswer
walkNode cfg seed (layer, mMyRef) = do
  realized <- case NE.nonEmpty (layerBranches layer) of
    Nothing -> pure (retagFinish layer)
    Just childBranches -> do
      answered <- walkGroup cfg (fmap (.value) childBranches)
      pure (rebuildLayer layer (NE.zipWith (\br a -> Th.Branch br.brief a) childBranches answered))
  foldAt seed.seedPath (fromMaybe seed.seedRef mMyRef) realized

-- | A childless layer carries no branch value to convert — total by the
-- same construction 'Tidepool.Thought.retagEmpty' relies on: 'layerBranches'
-- returns @[]@ ONLY for 'Th.Finish', whose one field never mentions the
-- branch element type at all, so nothing here is ever actually applied to
-- a value.
retagFinish :: ThoughtF a -> ThoughtF b
retagFinish layer = case layer of
  Th.Finish d -> Th.Finish d
  _ -> error "Harness.retagFinish: a layer with branches is not childless"

-- | Pure: does a budget already refuse @seed@ before any window would ever
-- run? Mirrors 'Tidepool.Thought.depthCapped' and this module's own
-- 'allowanceCapped' EXACTLY (same guard, same 'Th.Finish' text and origin)
-- — see 'discoverGroup''s doc for why re-deriving the same two conditions
-- here is safe rather than a second source of truth that could drift: the
-- real middleware chain re-checks the SAME seed fields, so a mismatch could
-- only ever waste a window call, never change which seeds end up capped.
preCapped :: Config -> NodeSeed -> Bool
preCapped cfg seed = seed.seedDepth >= cfg.maxDepth || seed.seedAllowance < 1

-- | ONE bulk window call per sibling group, windowing every UN-capped seed
-- and pairing the result back up with EVERY seed in the group, in original
-- order — a capped seed carries 'unresolvedOutcome', which 'discoverGroup''s
-- own middleware chain is guaranteed to never force (see its doc).
bulkDiscoverOutcomes ::
  Config ->
  NonEmpty NodeSeed ->
  Companion (NonEmpty (NodeSeed, Either InvocationExit (LayerProposal, ContextRef)))
bulkDiscoverOutcomes cfg seeds = do
  windowed <- case NE.nonEmpty toWindow of
    Nothing -> pure []
    Just needWindow ->
      let sharedRef = seedRef (NE.head needWindow)
       in NE.toList <$> bulkLayerWindow cfg sharedRef needWindow
  pure (NE.fromList (zipCapped (NE.toList seeds) windowed))
  where
    toWindow = filter (not . preCapped cfg) (NE.toList seeds)
    zipCapped [] _ = []
    zipCapped (s : ss) rs
      | preCapped cfg s = (s, unresolvedOutcome) : zipCapped ss rs
      | otherwise = case rs of
          r : rs' -> (s, r) : zipCapped ss rs'
          [] -> error "bulkDiscoverOutcomes: fewer window results than un-capped seeds"

unresolvedOutcome :: Either InvocationExit (LayerProposal, ContextRef)
unresolvedOutcome =
  error "bulkDiscoverOutcomes: a budget-capped seed's window outcome must never be forced"

-- | The seed-carried node-count cap.
--
-- 'Tidepool.Thought.nodeCapped' is the same cap over @MonadState Int@, and
-- the outer row is not a @MonadState@ stack — so the allowance rides on the
-- seed instead: a node reserves one unit for itself and divides the remainder
-- among its children ('childAllowance').  Same shape, same determinism
-- guarantee, and completion order cannot reach it.
--
-- @rootAllowance@ (the run's own 'Config.maxNodes') rides along purely to
-- give the forced 'Th.ForcedNodeCount' reason a governing number to report
-- alongside the exhausted seed allowance ('Th.renderForcedReason') — it is
-- never itself the guard; @seed.seedAllowance < 1@ is.
allowanceCapped :: Int -> Coalg Companion NodeSeed -> Coalg Companion NodeSeed
allowanceCapped rootAllowance inner seed
  | seed.seedAllowance < 1 =
      let reason = Th.ForcedNodeCount seed.seedAllowance rootAllowance
       in pure (Th.Finish (Th.Draft (Th.forcedDraftText reason) (Th.BudgetForced reason) seed.seedDepth))
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
-- The seed it is handed is the one 'discoverWith' re-stamped with this
-- node's own post-coalgebra ref, so the child seeds it builds fork the
-- window that just produced this very proposal.
--
-- Note what the layer CARRIES: 'Th.Concurrent', unconditionally — sibling
-- branch windows are ALWAYS driven concurrently now (operator decision:
-- scheduling is never a model-visible choice, so there is no PROPOSED value
-- to stamp here, only the one true answer).
layerFromProposal :: NodeSeed -> LayerProposal -> ThoughtF NodeSeed
layerFromProposal seed proposal = case proposal of
  ProposeFinish {localAnswer = t} -> Th.Finish (Th.Draft t Th.ModelFinished seed.seedDepth)
  ProposeSplit
    { splitPosture = po
    , splitFocus = f
    , splitBranches = pbs
    } -> splitLayer seed po f pbs

splitLayer :: NodeSeed -> Posture -> Text -> [ProposedBranch] -> ThoughtF NodeSeed
splitLayer seed po f pbs
  | null pbs = invocationFailed seed "the window proposed a split with no branches"
  | any blank pbs = invocationFailed seed "the window proposed a branch with a blank title or instruction"
  | otherwise = postureLayer po f (NE.fromList (imap child pbs)) Th.Concurrent
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
  ExecTimeout detail -> "timed out: " <> detail

-- | Every field a fold's own 'FoldOutcome' contributes — a named record
-- (companion review step 4) so a sibling lane extending 'foldAt'
-- (node-worktree-tree) has named fields to grow rather than a tuple
-- position to renumber.
data FoldOutcome = FoldOutcome
  { outcomeSynthesis :: Text
  , outcomeTensions  :: [Text]
  , outcomeBadges    :: [Text]
  , outcomeFailed    :: Int
  }

-- | Fold one node. Every child in @realized@ has ALREADY been walked —
-- discovered, recursed into, and folded — by the time this runs
-- ('walkNode' builds @realized@ before calling this), so there is no
-- deferred continuation left to force: this is a plain, one-shot
-- computation over an already-realized layer, not a closure waiting on its
-- own path.
--
-- @ref@ is the ContextRef this node's own fold branches from — its own
-- post-coalgebra ref when it has one, else its own pre-coalgebra ref
-- ('walkNode' resolves which). Used only when this node actually opens a
-- fold window ('mechanicalLeaf' below is 'False'); a mechanical leaf never
-- touches it.
foldAt :: NodePath -> ContextRef -> ThoughtF NodeAnswer -> Companion NodeAnswer
foldAt path ref realized = do
  let kids = layerBranches realized
      kidAnswers = map (.value) kids
      isRoot = path == NodePath []
  ownBranch <- ownDelegatedBranch path
  (mergeBranch, mergeNotes) <- mergeFold path ownBranch kids
  case mergeNotes of
    [] -> pure ()
    _ -> record "merge" key (object ["branch" .= mergeBranch, "steps" .= mergeNotes])
  -- A childless non-root node folds MECHANICALLY — no fold window at all
  -- (operator decision, 2026-08-20). A leaf's fold prompt held nothing but
  -- the node's own just-finished answer, and observed leaf folds simply
  -- rewrote that answer (4 of 10 windows in the first interaction-surface
  -- turn were leaf folds): the finish IS the synthesis, and integration is
  -- the PARENT fold's job — it sees every child's answer. The ROOT keeps
  -- its fold window even when childless — "no special case" means the SAME
  -- mechanism, branched off whatever ref it has, not the same OUTCOME as
  -- every other node.
  let mechanicalLeaf = null kids && not isRoot
  (fo, algebraExit) <-
    if mechanicalLeaf
      then
        pure
          ( FoldOutcome
              { outcomeSynthesis = leafAnswerText realized
              , outcomeTensions = []
              , outcomeBadges = []
              , outcomeFailed = 0
              }
          , Nothing
          )
      else do
        outcome <- foldWindow ref (algebraPrompt path realized kids mergeNotes)
        -- A FOLD window that exits is this node's OWN failure, and it must not be
        -- its subtree's.  The children below it already ran and already folded;
        -- discarding their answers, their tree lines, or their accounting here
        -- would erase completed sibling work one level up — the same erasure
        -- decision 6 forbids at a branch position, just reached from the algebra
        -- side.  So the exit replaces only what this node itself was going to
        -- contribute (its synthesis and tensions), and everything the children
        -- earned rolls up untouched.
        pure $ case outcome of
          Right (fd, _postFoldRef) ->
            ( FoldOutcome
                { outcomeSynthesis = fd.foldSynthesis
                , outcomeTensions = fd.foldTensions
                , outcomeBadges = []
                , outcomeFailed = 0
                }
            , Nothing
            )
          Left e ->
            ( FoldOutcome
                { outcomeSynthesis =
                    [fmt|<this node's fold window exited: {renderInvocationExit e}> — its {show (length kids)} branch result(s) are below, unfolded|]
                , outcomeTensions = []
                , outcomeBadges = ["fold failed"]
                , outcomeFailed = 1
                }
            , Just e
            )
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
  case failureReason realized of
    Nothing -> pure ()
    Just why -> record "failed" key (object ["reason" .= why, "window" .= ("coalgebra" :: Text)])
  case algebraExit of
    Nothing -> pure ()
    Just e ->
      record "failed" key (object ["reason" .= renderInvocationExit e, "window" .= ("algebra" :: Text)])
  pure
    NodeAnswer
      { answerPath = path
      , answerPosture = layerPosture realized
      , answerSynthesis = fo.outcomeSynthesis
      , answerTensions = fo.outcomeTensions
      , answerBadges = originBadges realized <> fo.outcomeBadges
      , answerTree = concatMap childLines kids
      , answerNodes = 1 + sum (map (.answerNodes) kidAnswers)
      , answerWindows =
          selfWindows realized
            - (if mechanicalLeaf then 1 else 0)
            + sum (map (.answerWindows) kidAnswers)
      , answerForced = selfForced realized + sum (map (.answerForced) kidAnswers)
      , answerFailed = selfFailed realized + fo.outcomeFailed + sum (map (.answerFailed) kidAnswers)
      , answerMergeBranch = mergeBranch
      }
  where
    key = renderPath path
    childLines (Th.Branch b a) = subtreeLines b.title a

-- | How many MODEL windows this node itself spent.  A node a budget refused
-- before its coalgebra ran spent only its fold; every other node — including
-- one the fan-out cap refused, whose coalgebra HAD run — spent two.
selfWindows :: ThoughtF a -> Int
selfWindows layer = case layer of
  Th.Finish d -> case d.draftOrigin of
    Th.BudgetForced Th.ForcedDepth {} -> 1
    Th.BudgetForced Th.ForcedNodeCount {} -> 1
    _ -> 2
  _ -> 2

-- | A childless node's answer text — its coalgebra's own finish draft. Only
-- a 'Th.Finish' layer can be childless ('splitLayer' turns an empty split
-- into a forced finish), so the fallback arm is unreachable in practice
-- but stays legible rather than partial.
leafAnswerText :: ThoughtF a -> Text
leafAnswerText layer = case layer of
  Th.Finish d -> d.draftText
  _ -> "<childless layer with no finish draft>"

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
-- Why this is a wrapper and not a line inside 'discoverWith': `discoverWith`
-- runs INNERMOST, so a layer it journaled could still be discarded by the
-- fan-out cap or reshaped by the gate.  Journaling there made kind @split@ name
-- children that never ran (a fan-out-capped node) and miss children the
-- operator added (a gated one) — and §10.1 promises that a @split@ entry's
-- child paths ARE the durable record of the tree's shape.  A resume fold
-- reading it would have rebuilt a tree the run never had.
--
-- So the two facts get two kinds, and neither is a summary of the other:
-- @proposed@ is what the WINDOW said (written by 'discoverWith', the
-- friction log's raw material — a model's refused 7-branch layer is exactly
-- what C6 wants to see), and @split@\/@finish@ is what the DRIVER did.  A
-- capped node honestly carries both: a @proposed@ naming seven branches and
-- a @finish@ stamped @BudgetForced ForcedFanOut@.
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
          , "branches" .= map branchEntry (layerBranches layer)
          ]
      )
  where
    key = renderPath seed.seedPath
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
-- parent's frozen context ('bulkLayerWindow'), so the ancestry is the
-- window's own shared prefix rather than a summary of it rendered into a
-- suffix.
-- ---------------------------------------------------------------------------

-- | A complete, minimal, COMPILABLE @finalize@ example (companion review
-- step 8f) — teaching constructor nesting and required-empty-list fields
-- better than schema prose alone. Delimited with a plain @--- EXAMPLE ---@
-- marker rather than a triple-backtick fence: a markdown fence here would be
-- the FIRST one in the assembled request, ahead of the engine's own
-- auto-rendered hole-card type shape, and 'companion_recursive_slice.rs''s
-- row-1 check reads exactly that FIRST fenced block (@fenced_haskell@) —
-- this example must never compete with it for that position. Only the
-- algebra's own example lives here now — the coalgebra's pair moved to
-- 'HarnessTypes.render' alongside the rest of the one-off teaching; see
-- 'coalgebraPrompt's doc.
exampleBlock :: Text -> Text
exampleBlock code =
  [fmt|--- EXAMPLE ---
{code}
--- END EXAMPLE ---|]

exampleFoldDecisionEmpty :: Text
exampleFoldDecisionEmpty =
  exampleBlock
    "finalize @FoldDecision (FoldDecision { foldSynthesis = \"what this node concludes\", foldTensions = [] })"

-- | The numeric budget line every child brief carries (companion review
-- run-4 finding, P1: "it wants per-node REMAINING depth/slots/rounds" —
-- this is the mechanical subset: the numbers a coalgebra window needs to
-- see its own room to fork, read straight off the seed and 'Config' rather
-- than left implicit).
--
-- MINIMAL AT EVERY DEPTH, root included — no depth branch, no special case
-- ('rootSeed's doc: "the root is not a special case anywhere below it").
-- The mechanics prose (finalize\/delegate\/askUser contracts, the two
-- compilable examples) used to be re-taught in full at every node: every
-- non-root coalgebra window is a 'bulkLayerWindow' BRANCH off its own
-- parent's frozen prefix, and every ancestor was itself a branch off ITS
-- parent, so a depth-N node's inherited context already contained N copies
-- of that teaching before its own fresh copy added an (N+1)th. It now lives
-- ONCE per turn, in 'HarnessTypes.render''s output — the per-cycle system
-- framing every coalgebra\/algebra window in the tree inherits (root
-- included, since 'loop' freezes its own context, which carries that
-- framing, before minting 'rootSeed') — so this prompt only ever needs to
-- say what is genuinely per-node: where this node sits, its budget, and its
-- own instruction.
coalgebraPrompt :: Config -> NodeSeed -> Text
coalgebraPrompt cfg seed =
  [fmt|NODE {renderPath seed.seedPath} — DISCOVER.

Budget: depth {seed.seedDepth} of {cfg.maxDepth} max, node allowance {seed.seedAllowance}, fan-out cap {cfg.maxFanOut}.

{renderBrief seed.seedBrief}

Finalize: `finalize @LayerProposal (...)`|]

-- | The prompt carries exactly three things — the @NODE <path> — FOLD@
-- header (KEEP this exact format;
-- 'companion_recursive_slice.rs''s scripted provider keys every window on
-- it), each child's own FINAL result (a leaf child's localAnswer, a split
-- child's own fold synthesis — 'childSummary' below), and the delegate
-- merge-notes block when present ('mergeNotes', PRD 21 lane C5's merge
-- fold, already run by the time this prompt is built — 'foldAt' calls
-- 'mergeFold' before 'foldWindow'). NO re-orientation prose: this window is
-- a BRANCH off the node's own DISCOVER turn now, so its ancestry — what
-- this node is, what it asked its children to do — is already in its own
-- inherited context, not re-taught here. Rendered ABOVE the branch block on
-- purpose: 'companion_recursive_slice.rs''s @branch_summary@ reads a
-- branch's own text up to the NEXT "--- branch " or the literal "\\n\\nFold
-- this realized layer" marker, so nothing may sit between the LAST branch's
-- block and that marker without silently widening what a sibling's "own"
-- summary is read to contain. @mergeSection@ carries its own leading blank
-- line and is the empty string when @mergeNotes@ is @[]@, so a node with no
-- worktree content renders byte-identical to one that never touched git.
algebraPrompt :: NodePath -> ThoughtF NodeAnswer -> [Th.Branch NodeAnswer] -> [Text] -> Text
algebraPrompt path realized kids mergeNotes =
  [fmt|NODE {renderPath path} — FOLD.

{childBlock}
{mergeSection}

Fold this realized layer into one answer. The branches are in DECLARED order,
never completion order, and a branch that failed is an ordinary value in the
list — say what it cost you rather than pretending it did not happen.

Finalize a FoldDecision {{ foldSynthesis, foldTensions }}:
`foldSynthesis` is this node's answer as prose, written to be read on its own;
`foldTensions` are the disagreements the branches did NOT resolve, one per
entry, kept rather than averaged away.

Example:

{exampleFoldDecisionEmpty}

Finalize: `finalize @FoldDecision (...)`|]
  where
    -- A childless fold (reachable only for the ROOT — every other childless
    -- node folds mechanically, no window at all) still needs SOMETHING to
    -- fold: its own local finish, in a delimited block.
    childBlock = case kids of
      [] -> localFinishBlock (leafAnswerText realized)
      _ -> T.intercalate "\n\n" (map childSummary kids)
    mergeSection = case mergeNotes of
      [] -> "" :: Text
      ns -> "\nWorktree merges into this node:\n" <> T.intercalate "\n" (map ("- " <>) ns)

-- | The local result a childless fold is actually folding, in a delimited
-- block — never just a description that one exists.
localFinishBlock :: Text -> Text
localFinishBlock t =
  [fmt|This node has no branches: it finished locally. Its own local result:
--- BEGIN LOCAL FINISH ---
{t}
--- END LOCAL FINISH ---|]

-- | One child's own final result — a leaf's localAnswer or a split node's
-- own fold synthesis, either way just 'answerSynthesis' (both routes set it
-- to exactly that). NO instruction line: the instruction this branch was
-- given is already in this node's OWN inherited context (its own earlier
-- ProposeSplit), so repeating it here would be exactly the re-orientation
-- prose branching-off-the-node's-own-conversation makes redundant.
childSummary :: Th.Branch NodeAnswer -> Text
childSummary (Th.Branch b a) =
  [fmt|--- branch {renderPath a.answerPath}: {b.title} ({show b.role}) [{a.answerPosture}]
{a.answerSynthesis}{tensions}|]
  where
    tensions = case a.answerTensions of
      [] -> "" :: Text
      ts -> "\nunresolved: " <> T.intercalate "; " ts
