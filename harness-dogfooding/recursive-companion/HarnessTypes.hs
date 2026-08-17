{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Durable vocabulary for the recursive-companion dogfood (PRD 21 lane C3,
-- @plans\/self-iterating-harness\/21-c3-recursive-companion-slice.md@).
--
-- __Why the answer types live HERE and not beside @loop@.__
-- @HarnessSource::answerer_imports@ derives a window's in-scope modules
-- structurally from the SIBLING modules the harness file imports, and the
-- harness module itself is never among them (it defines @loop@, whose
-- @runLLMTurn*@ verbs are absent from the answerer's row, and GHC compiles an
-- imported module whole).  A type inlined beside @loop@ therefore cannot be
-- named by the window that is asked to finalize it, and the failure looks
-- like a model hallucinating a type rather than a missing import.  So
-- 'LayerProposal', 'FoldProposal' and 'LayerApproval' — every type a window
-- or a form has to name — live in this module.
--
-- __Everything here is pure.__  No effect, no handle, no @loop@: the two
-- decisions that turn a model's answer into a layer ('layerFromProposal') and
-- an operator's verdict into an amended layer ('applyGate') are total
-- functions a test calls directly with no model and no operator anywhere in
-- the path.
module HarnessTypes
  ( -- * Checkpointed state
    State (..)
  , Config (..)
  , RunSummary (..)
  , initialState

    -- * Node identity
  , NodePath (..)
  , slug
  , renderPath
  , childPath
  , parentPath
  , pathDepth

    -- * The seed (the hylo's @a@)
  , NodeSeed (..)
  , InheritedContext (..)
  , renderInherited
  , renderBrief
  , utf8Bytes
  , childAllowance
  , childSeed

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
  , gateApplies

    -- * The pure decisions
  , layerFromProposal
  , applyGate

    -- * Layer plumbing (pure, order-preserving)
  , layerBranches
  , rebuildLayer
  , indexLayer
  , layerStrategy
  , layerPosture
  , executedStrategy
  , strategyBadges
  , renderOrigin

    -- * What a node folds to
  , NodeAnswer (..)
  , failureAnswer
  , nodeLine
  , subtreeLines

    -- * Rendering
  , render
  ) where

import qualified Data.List.NonEmpty as NE
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
-- The base functor and its budget vocabulary are consumed VERBATIM from the
-- C0 fixture.  Its CONSTRUCTORS are reached qualified because three of them
-- (@Explore@\/@Compare@\/@Challenge@) share their names with this module's
-- 'Posture' wire enum, and @BranchRole@ shares all three of its with
-- 'BranchRoleWire' — the wire names are what a model writes, so those are the
-- ones that stay unqualified.
import Tidepool.Thought (BranchRole, Draft, ForkBrief, Strategy, ThoughtF)
import qualified Tidepool.Thought as Th

-- ---------------------------------------------------------------------------
-- Checkpointed state
-- ---------------------------------------------------------------------------

-- | Only durable facts cross a turn boundary: the question, the budget, and
-- the last folded run.  Live contexts, parked continuations and partial trees
-- are not checkpointed (PRD 21, "Persistence (v1)") — a process loss mid-turn
-- reruns the turn from here.
data State = State
  { question  :: Text
  , config    :: Config
  , turnCount :: Int
  , lastRun   :: Maybe RunSummary
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | Three hard caps plus the gate's policy and its round bound.  Enforced,
-- never advisory: this is what the middleware in @Harness.loop@ reads.
data Config = Config
  { maxDepth      :: Int
  , maxNodes      :: Int
  , maxFanOut     :: Int
  , gatePolicy    :: GatePolicy
  , -- | How many times the layer-approval form may be re-presented for ONE
    -- layer before the layer proceeds as last amended (default 8, matching
    -- @ASKUSER_MAX_REPROMPTS@'s spirit).  Hitting the bound is journaled.
    gateMaxRounds :: Int
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | What @render@ shows about the last turn.  Flat by construction — the tree
-- is a list of already-rendered lines, not a second tree to walk.
data RunSummary = RunSummary
  { runAnswer   :: Text
  , runTensions :: [Text]
  , runTree     :: [Text]
  , runNodes    :: Int
  , runWindows  :: Int
  , runForced   :: Int
  , runFailed   :: Int
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

initialState :: State
initialState =
  State
    { question =
        "What should the recursive companion's first real dogfood scenario be, \
        \and what would it have to show to be worth running attended?"
    , config =
        Config
          { maxDepth = 3
          , maxNodes = 12
          , maxFanOut = 4
          , gatePolicy = GateWiderThan {gateWidth = 3}
          , gateMaxRounds = 8
          }
    , turnCount = 0
    , lastRun = Nothing
    }

-- ---------------------------------------------------------------------------
-- Node identity
-- ---------------------------------------------------------------------------

-- | Root-relative branch slugs, outermost first.  The root is @NodePath []@.
--
-- This is the node's identity EVERYWHERE — the journal key, the render's tree
-- line, the needle a scripted provider matches a window's prompt on.  One
-- name, one derivation ('childPath'), never re-spelled per consumer.
newtype NodePath = NodePath [Text]
  deriving (Show, Eq)

pathDepth :: NodePath -> Int
pathDepth (NodePath segs) = length segs

-- | @"root"@, @"root\/1-cheaper-index"@, …
renderPath :: NodePath -> Text
renderPath (NodePath segs) = case segs of
  [] -> "root"
  _ -> "root/" <> intercalate "/" segs

-- | Drop the last segment.  Total: the root's parent is the root.  Used by
-- 'applyGate', which must re-derive a whole layer's child paths from a
-- surviving sibling after the operator pruned or added one.
parentPath :: NodePath -> NodePath
parentPath (NodePath segs) = NodePath (take (max 0 (length segs - 1)) segs)

-- | A child at zero-based branch index @i@ extends its parent by exactly one
-- segment: @\<i+1\>-\<slug title\>@.  The leading index makes siblings unique
-- even when two titles slug identically, and makes branch ORDER readable in
-- every receipt.
--
-- ONE function, called by BOTH the coalgebra (building child seeds) and the
-- algebra (applying child folders), so the two cannot disagree about a
-- child's identity.  They agree by construction: both see the same 'Th.Branch'
-- list, in the same order, carrying the same brief title — the algebra reads
-- the very 'Th.ForkBrief' the coalgebra put there.
childPath :: NodePath -> Int -> Text -> NodePath
childPath (NodePath segs) i title = NodePath (segs <> [show (i + 1) <> "-" <> slug title])

-- | Lowercase, every run of non-@[a-z0-9]@ to a single @-@, trimmed, capped
-- at 32 characters; a title that slugs empty becomes @branch@.
--
-- __A containment requirement, not cosmetics.__  @tidepool-web@'s loopback
-- trust model rests on "@node_id@ is always a substrate identifier a caller
-- passed to @register_node@, never model-produced text" — and a branch title
-- IS model-produced text.  Slugging to @[a-z0-9-]{1,32}@ under an integer
-- prefix is what keeps that invariant true when the tree's node ids are
-- derived from a model's own branch labels.
slug :: Text -> Text
slug title =
  let squashed = trimDashes (collapse (map keep (unpack (toLower title))))
      capped = trimDashes (take 32 squashed)
   in if null capped then "branch" else pack capped
  where
    keep c = if (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') then c else '-'
    collapse s = case s of
      [] -> []
      ('-' : rest) -> '-' : collapse (dropWhile (== '-') rest)
      (c : rest) -> c : collapse rest
    trimDashes = reverse . dropWhile (== '-') . reverse . dropWhile (== '-')

-- ---------------------------------------------------------------------------
-- The seed
-- ---------------------------------------------------------------------------

-- | What a node needs in order to be unfolded.  The hylo's @a@.
--
-- No live handle rides here (unlike @dev-tree@'s seed, which carries a
-- @WorktreeHandle@ and therefore has to live beside @loop@): every field is
-- ordinary data, which is why the seed and the two pure decisions that build
-- and amend layers can share this module.
data NodeSeed = NodeSeed
  { seedPath      :: NodePath
  , seedBrief     :: ForkBrief
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
  , seedContext   :: InheritedContext
  }
  deriving (Show, Eq)

-- | What v1 actually sends down.
--
-- __This is PROMPT-RENDERED inheritance, not a frozen snapshot.__  PRD 21
-- locked decision 2 wants every child to fork the frozen post-coalgebra
-- context; that is not reachable from the authored surface today
-- (@Harness::freeze_snapshot@\/@fork_from_snapshot@ have no production caller,
-- and a window has no Haskell-visible identity to branch FROM), so the shared
-- prefix here is text the driver composes.  Consequences, stated so no
-- receipt overclaims them: there is no digest to record (the journal records
-- 'utf8Bytes' of the rendered inheritance and nothing digest-shaped), and the
-- "enormous shared prefix, tiny divergent suffix" cache shape is NOT
-- demonstrated by this slice.  Sibling isolation holds anyway, for an
-- unrelated reason: each child window is a freshly minted node.
data InheritedContext = InheritedContext
  { -- | One rendered line per ancestor: where it sat, and the brief of the
    -- branch this node descends through.
    inheritedAncestry :: [Text]
  , -- | The PARENT's coalgebra decision, rendered.
    inheritedDecision :: Text
  }
  deriving (Show, Eq)

renderInherited :: InheritedContext -> Text
renderInherited ctx = T.intercalate "\n" (ancestrySection <> decisionSection)
  where
    ancestrySection = case ctx.inheritedAncestry of
      [] -> []
      ls -> "Where you are in the tree:" : map ("  " <>) ls
    decisionSection =
      if ctx.inheritedDecision == ""
        then []
        else ["", "Your parent's decision, in full:", ctx.inheritedDecision]

renderBrief :: ForkBrief -> Text
renderBrief b =
  [fmt|Your branch: {b.title} ({show b.role})
{b.instruction}|]

-- | Exact UTF-8 byte length.  There is no @Data.Text.Encoding@ on this
-- surface, and the journal's @inheritedBytes@ has to be a real byte count
-- rather than a character count — it is the only honest measure of what this
-- slice actually sends down, and it is deliberately NOT digest-shaped.
utf8Bytes :: Text -> Int
utf8Bytes t = sum (map width (unpack t))
  where
    width c
      | n < 0x80 = 1
      | n < 0x800 = 2
      | n < 0x10000 = 3
      | otherwise = 4
      where
        n = ord c

-- | Divide what is left after this node's own reservation among its children.
--
-- Conservative on purpose (a subtree finishing under its share does not
-- return the remainder to its siblings) and NEVER clamped upward: a share
-- that floors to zero stays zero, so a node that cannot fund every child
-- hands the underfunded ones nothing rather than minting nodes the parent
-- does not have.  @Harness.allowanceCapped@ turns that zero into a typed
-- @BudgetForced ForcedNodeCount@ finish for the child.
childAllowance :: NodeSeed -> Int -> Int
childAllowance parent n
  | n <= 0 = 0
  | otherwise = max 0 (parent.seedAllowance - 1) `div` n

-- | THE inheritance seam — the ONE function that decides what a child knows
-- from above.
--
-- When the frozen-snapshot gap closes (a window's own frozen digest returned
-- alongside its answer, accepted as an optional branch-of parameter on the
-- next window), THIS is the single function that changes: 'seedContext' stops
-- being composed text and becomes the parent's snapshot reference.  Nothing
-- else in the harness reads 'InheritedContext' except 'renderInherited' at
-- the prompt site.
childSeed :: NodeSeed -> Text -> Int -> Int -> ForkBrief -> NodeSeed
childSeed parent decision allowance i b =
  NodeSeed
    { seedPath = childPath parent.seedPath i b.title
    , seedBrief = b
    , seedDepth = parent.seedDepth + 1
    , seedAllowance = allowance
    , seedContext =
        InheritedContext
          { inheritedAncestry = parent.seedContext.inheritedAncestry <> [ancestryLine parent b]
          , inheritedDecision = decision
          }
    }

ancestryLine :: NodeSeed -> ForkBrief -> Text
ancestryLine parent b =
  [fmt|{renderPath parent.seedPath} ({parent.seedBrief.title}) -> {b.title} ({show b.role})|]

-- ---------------------------------------------------------------------------
-- What a window may finalize
-- ---------------------------------------------------------------------------

-- | The COALGEBRA's answer.  ONE layer, and NO RECURSIVE ARM — that is the
-- type-level guarantee behind PRD 21 locked decision 1 ("the root is never
-- asked for descendant shape").  A coalgebra invocation is structurally
-- incapable of describing more than its own layer, so a model that tries to
-- hand back a whole tree produces a decode error, not a deeper tree.
data LayerProposal
  = ProposeFinish {finishDraft :: Text}
  | ProposeSplit
      { splitPosture  :: Posture
      , -- | The focus, the decision, or the claim — per the posture.
        splitFocus    :: Text
      , splitStrategy :: ProposedStrategy
      , -- | A plain list, not a 'NE.NonEmpty': the wire shape a model writes
        -- must be an ordinary JSON array, and emptiness is a CONDITION the
        -- driver detects ('layerFromProposal') rather than a shape the model
        -- is trusted to respect.
        splitBranches :: [ProposedBranch]
      }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

data ProposedBranch = ProposedBranch
  { branchTitle       :: Text
  , branchRole        :: BranchRoleWire
  , branchInstruction :: Text
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | The three postures 'ThoughtF' offers, as the enum a model writes.  Named
-- separately from @Tidepool.Thought@'s own constructors because a wire enum
-- and a base-functor constructor are different things that happen to agree
-- today; 'postureLayer' is the one place they are mapped.
data Posture = Explore | Compare | Challenge
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | What a model may ASK for.  Recorded and rendered, never scheduled in v1 —
-- see 'executedStrategy'.
--
-- @WantPooled@ takes a NAMED field rather than a positional one: a payload
-- constructor in a sum must use record syntax, because the vendored generic
-- JSON has no key to put a positional field under and rejects it with a
-- compile-time @TypeError@.
data ProposedStrategy
  = WantSequential
  | WantConcurrent
  | WantPooled {pooledWidth :: Int}
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

data BranchRoleWire = Primary | Alternative | Critic
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | The ALGEBRA's answer.  Deliberately thin in v1: artifact selection and
-- composition are lane C4's, and PRD 21 open question 2 says this type
-- shrinks freely until persistence depends on it.
data FoldProposal = FoldProposal
  { foldSynthesis :: Text
  , foldTensions  :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

roleOf :: BranchRoleWire -> BranchRole
roleOf r = case r of
  Primary -> Th.Primary
  Alternative -> Th.Alternative
  Critic -> Th.Critic

-- ---------------------------------------------------------------------------
-- The gate
-- ---------------------------------------------------------------------------

-- | When the operator is asked to approve a layer.
--
-- 'GateOff' is what the scripted acceptance tier runs under, and it is a real
-- configuration rather than a test hook — an unattended companion turn is a
-- legitimate mode, and under it NO suspension is raised at all.
--
-- @GateWiderThan@ carries a NAMED field for the same reason 'WantPooled'
-- does.
data GatePolicy
  = GateOff
  | GateWiderThan {gateWidth :: Int}
  | GateEveryLayer
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

data GateVerdict = Approve | Prune | Amend | Add
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | The gate's form.  ONE record, so it renders as one form rather than a
-- variant chooser; the fields a verdict does not use are left empty.
data LayerApproval = LayerApproval
  { gateVerdict :: GateVerdict
  , -- | The branch title the verdict applies to (@""@ for 'Approve'\/'Add').
    gateTarget  :: Text
  , -- | 'Add': the new branch's title.
    gateTitle   :: Text
  , -- | 'Add': the new branch's role.
    gateRole    :: BranchRoleWire
  , -- | 'Amend'\/'Add': the instruction.
    gateText    :: Text
  , gateNote    :: Text
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | Whether this policy wants THIS layer presented.  A 'Th.Finish' is never
-- gated: there is no descent to approve.
gateApplies :: GatePolicy -> ThoughtF a -> Bool
gateApplies policy layer = case policy of
  GateOff -> False
  GateEveryLayer -> width > 0
  GateWiderThan {gateWidth = n} -> width > n
  where
    width = Th.branchCount layer

-- ---------------------------------------------------------------------------
-- Layer plumbing — pure, order-preserving
-- ---------------------------------------------------------------------------

layerBranches :: ThoughtF a -> [Th.Branch a]
layerBranches layer = case layer of
  Th.Finish _ -> []
  Th.Explore _ bs _ -> NE.toList bs
  Th.Compare _ os _ -> NE.toList os
  Th.Challenge _ as _ -> NE.toList as

-- | Put a (non-empty) branch list back under the same posture, at whatever
-- element type the caller now holds.  Callers guarantee non-emptiness —
-- 'applyGate' refuses to prune the last branch, and 'layerFromProposal' turns
-- an empty proposal into a 'Th.Finish' before this is ever reached.
rebuildLayer :: ThoughtF a -> [Th.Branch b] -> ThoughtF b
rebuildLayer layer brs = case layer of
  Th.Finish d -> Th.Finish d
  Th.Explore f _ s -> Th.Explore f (NE.fromList brs) s
  Th.Compare d _ s -> Th.Compare d (NE.fromList brs) s
  Th.Challenge c _ s -> Th.Challenge c (NE.fromList brs) s

-- | Attach each branch's zero-based position to its value, preserving order.
-- The descent needs the index to rebuild a child's 'NodePath', and
-- @Harness.traverseLayer@'s callback takes a bare branch — so the index rides
-- in the branch's own value rather than in a second, order-coupled list.
indexLayer :: ThoughtF a -> ThoughtF (Int, a)
indexLayer layer = rebuildLayer layer (imap tag (layerBranches layer))
  where
    tag i (Th.Branch b v) = Th.Branch b (i, v)

-- | The strategy the layer CARRIES, which for a split is the one the model
-- PROPOSED — see 'executedStrategy'.
layerStrategy :: ThoughtF a -> Strategy
layerStrategy layer = case layer of
  Th.Finish _ -> Th.Sequential
  Th.Explore _ _ s -> s
  Th.Compare _ _ s -> s
  Th.Challenge _ _ s -> s

layerPosture :: ThoughtF a -> Text
layerPosture layer = case layer of
  Th.Finish d -> "finish(" <> renderOrigin d <> ")"
  Th.Explore f _ _ -> "explore: " <> f
  Th.Compare d _ _ -> "compare: " <> d
  Th.Challenge c _ _ -> "challenge: " <> c

-- | What the driver actually runs, always.
--
-- 'Tidepool.Thought.thoughtHylo' descends through @traverse@, and the one
-- concurrent primitive the authored loop has today (@runLLMTurnFanout@) fans
-- out one WINDOW per prompt and retires each at finalize — a window cannot
-- host a recursive subtree, so it can parallelize a layer of windows but
-- never a layer of SUBTREES.  Every proposed 'Strategy' therefore executes as
-- 'Th.Sequential'.
--
-- That is PRD 21 locked decision 9's EXPLICIT transformation, not a silent
-- downgrade: 'strategyBadges' stamps it on the node and 'render' shows it.
executedStrategy :: Strategy -> Strategy
executedStrategy _proposed = Th.Sequential

-- | @["strategy: proposed Concurrent, executed Sequential"]@ when the model
-- asked for something the driver does not run; @[]@ when it did not.
strategyBadges :: Strategy -> [Text]
strategyBadges proposed
  | proposed == executed = []
  | otherwise = [[fmt|strategy: proposed {show proposed}, executed {show executed}|]]
  where
    executed = executedStrategy proposed

-- | Why a node finished, rendered — so a budget-forced finish and a
-- model-chosen one are distinguishable in every receipt without a second
-- journal kind.
renderOrigin :: Draft -> Text
renderOrigin d = case d.draftOrigin of
  Th.ModelFinished -> "model"
  Th.BudgetForced r -> "forced " <> show r
  Th.InvocationFailed f -> "failed: " <> f.failureReason

-- ---------------------------------------------------------------------------
-- The pure decisions
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
-- Note what the layer CARRIES: the model's PROPOSED strategy, unmodified.
-- The transformation to 'Th.Sequential' happens at the descent
-- (@Harness.traverseLayer@), which is the one place that can also stamp it.
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
    decision = renderDecision po f ps pbs
    allowance = childAllowance seed (length pbs)
    child i pb =
      let b = Th.ForkBrief pb.branchTitle (roleOf pb.branchRole) pb.branchInstruction
       in Th.Branch b (childSeed seed decision allowance i b)

invocationFailed :: NodeSeed -> Text -> ThoughtF NodeSeed
invocationFailed seed why =
  Th.Finish (Th.Draft why (Th.InvocationFailed (Th.NodeFailure why)) seed.seedDepth)

postureLayer :: Posture -> Text -> NE.NonEmpty (Th.Branch a) -> Strategy -> ThoughtF a
postureLayer po f brs s = case po of
  Explore -> Th.Explore f brs s
  Compare -> Th.Compare f brs s
  Challenge -> Th.Challenge f brs s

strategyOfWire :: ProposedStrategy -> Strategy
strategyOfWire ps = case ps of
  WantSequential -> Th.Sequential
  WantConcurrent -> Th.Concurrent
  WantPooled {pooledWidth = n} -> Th.Pooled n

-- | The parent's coalgebra decision, rendered — what every child inherits as
-- 'inheritedDecision'.
renderDecision :: Posture -> Text -> ProposedStrategy -> [ProposedBranch] -> Text
renderDecision po f ps pbs =
  T.intercalate "\n" ([[fmt|{show po}: {f}|], [fmt|strategy proposed: {show ps}|]] <> imap line pbs)
  where
    line i pb =
      [fmt|  {show (i + 1)}. {pb.branchTitle} ({show pb.branchRole}) — {pb.branchInstruction}|]

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
-- * 'Add' — append a branch built from the form's title\/role\/text.
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
applyGate :: LayerApproval -> ThoughtF NodeSeed -> Either Text (ThoughtF NodeSeed)
applyGate approval layer = case layer of
  Th.Finish _ -> Right layer
  _ -> case approval.gateVerdict of
    Approve -> Right layer
    Prune
      | not (any titled brs) -> Left (noSuchBranch approval.gateTarget)
      | length brs <= 1 -> Left "pruning the last branch would leave the layer empty"
      | otherwise -> Right (reindexed (filter (not . titled) brs))
    Amend
      | not (any titled brs) -> Left (noSuchBranch approval.gateTarget)
      | otherwise -> Right (reindexed (map amend brs))
    Add
      | strip approval.gateTitle == "" -> Left "an added branch needs a title"
      | otherwise -> case brs of
          [] -> Left "a layer with no branches cannot take an added one"
          (template : _) -> Right (reindexed (brs <> [addedFrom template]))
  where
    brs = layerBranches layer
    titled br = br.brief.title == approval.gateTarget
    noSuchBranch t = [fmt|no branch in this layer is titled "{t}"|]
    amend br = if titled br then Th.Branch (amendedBrief br.brief) br.value else br
    amendedBrief b = b {Th.instruction = approval.gateText}
    -- An added branch inherits its siblings' depth, allowance and inherited
    -- context: it is a peer at the same position in the tree, and the
    -- operator adding it changes neither what this layer knows nor how much
    -- of the node budget the parent divided out.
    addedFrom template =
      Th.Branch
        (Th.ForkBrief approval.gateTitle (roleOf approval.gateRole) approval.gateText)
        template.value
    reindexed kept = rebuildLayer layer (imap repath kept)
    repath i (Th.Branch b s) =
      Th.Branch b s {seedPath = childPath (parentPath s.seedPath) i b.title}

-- ---------------------------------------------------------------------------
-- What a node folds to
-- ---------------------------------------------------------------------------

-- | One node's folded result, plus the subtree accounting the receipt reads.
--
-- 'answerTree' holds this node's DESCENDANTS' rendered lines, pre-order.  A
-- node's own line is built by whoever holds its brief — its parent, or
-- @Harness.loop@ for the root — because @ThoughtF@ has no task slot and an
-- algebra therefore never sees its own seed.  'subtreeLines' is that
-- composition.
data NodeAnswer = NodeAnswer
  { answerPath      :: NodePath
  , answerPosture   :: Text
  , answerSynthesis :: Text
  , answerTensions  :: [Text]
  , answerBadges    :: [Text]
  , answerTree      :: [Text]
  , answerNodes     :: Int
  , -- | Model windows spent in this subtree.  A node a budget refused BEFORE
    -- its coalgebra ran spends one (its fold); every other node spends two.
    answerWindows   :: Int
  , answerForced    :: Int
  , answerFailed    :: Int
  }
  deriving (Show, Eq)

-- | The answer a node folds to when nothing usable came back for it.
failureAnswer :: NodePath -> Text -> NodeAnswer
failureAnswer path why =
  NodeAnswer
    { answerPath = path
    , answerPosture = "failed"
    , answerSynthesis = why
    , answerTensions = []
    , answerBadges = ["failed"]
    , answerTree = []
    , answerNodes = 1
    , answerWindows = 1
    , answerForced = 0
    , answerFailed = 1
    }

-- | @\<indent\>\<path\>  \<posture\>  \<title\>  [badges]@ — one line per
-- node, never a nested transcript.
nodeLine :: Text -> NodeAnswer -> Text
nodeLine title a =
  T.replicate (pathDepth a.answerPath) "  "
    <> [fmt|{renderPath a.answerPath}  {a.answerPosture}  {title}|]
    <> badges
  where
    badges = case a.answerBadges of
      [] -> "" :: Text
      bs -> "  [" <> intercalate ", " bs <> "]"

-- | A subtree's lines: this node's own, then its descendants', pre-order.
subtreeLines :: Text -> NodeAnswer -> [Text]
subtreeLines title a = nodeLine title a : a.answerTree

-- ---------------------------------------------------------------------------
-- Render — the folded answer is primary
-- ---------------------------------------------------------------------------

-- | @render :: State -> Text@ — the LOCKED signature.  Domain policy only:
-- the driver composes this output with the loop-iteration count, the prior
-- compaction summary, and capability\/finalization instructions.
--
-- Emitted in order: the folded answer as prose, the tensions the root fold
-- surfaced, the tree, then a one-line receipt.  The tree is inspectable and
-- SUBORDINATE — BELOW the answer, not beside it, one line per node.
render :: State -> Text
render st = case st.lastRun of
  Nothing ->
    [fmt|You are a recursive companion. Nothing has folded yet.

Question: {st.question}
{budgetLine}

Your next turn discovers a tree one layer at a time: a coalgebra window
finalizes a LayerProposal for THIS node only (finish locally, or split into
the next layer's branches), each branch descends recursively from inherited
context, and an algebra window folds every realized layer back in declared
branch order.|]
  Just r ->
    [fmt|{r.runAnswer}
{tensionsBlock r}
--- tree ---
{T.intercalate "\n" r.runTree}

{receiptLine r}

Question: {st.question}
{budgetLine}
Turns folded: {show st.turnCount}|]
  where
    c = st.config
    budgetLine :: Text
    budgetLine =
      [fmt|Budget: depth {c.maxDepth}, {c.maxNodes} nodes, fan-out {c.maxFanOut}; gate {renderPolicy c.gatePolicy} (at most {c.gateMaxRounds} rounds)|]
    tensionsBlock r = case r.runTensions of
      [] -> "" :: Text
      ts -> "\nTensions:\n" <> T.intercalate "\n" (map ("- " <>) ts) <> "\n"
    -- Gate interventions are NOT counted here.  They happen in a node's
    -- coalgebra, and an algebra never sees its own seed (@ThoughtF@ has no
    -- task slot), so with @thoughtHylo@ used verbatim and no state effect in
    -- the outer row there is no channel that carries the count into the fold.
    -- The journal has every one of them, keyed by node path.
    receiptLine r =
      [fmt|receipt: {show r.runNodes} nodes, {show r.runWindows} windows, {show r.runForced} budget-forced finishes, {show r.runFailed} failures; gate interventions are journaled per node under kind "gate"|]

renderPolicy :: GatePolicy -> Text
renderPolicy p = case p of
  GateOff -> "off"
  GateEveryLayer -> "every layer"
  GateWiderThan {gateWidth = n} -> [fmt|layers wider than {show n}|]
