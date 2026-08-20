{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Durable vocabulary for the recursive-companion dogfood.
--
-- Every type here is nameable in the ANSWERER's row (@[AskUser, Fork,
-- ReadState, Finalize T]@, which has no @RunLLMTurn@): "Harness" defines
-- @loop@, whose @runLLMTurn*@ verbs are absent from that row, and GHC
-- compiles an imported module whole, so a window-facing type declared beside
-- @loop@ could not be named by the window asked to finalize it.
-- 'LayerProposal', 'FoldDecision' and 'LayerApproval' therefore live HERE;
-- 'NodeSeed' and the pure decisions over it ('layerFromProposal',
-- 'applyGate', @childSeed@) carry a @ContextRef@ (declared by @RunLLMTurn@'s
-- own decl) and so live in "Harness" instead.
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
  , pathDepth

    -- * The brief a node is prompted with
  , renderBrief

    -- * What a window may finalize
  , LayerProposal (..)
  , ProposedBranch (..)
  , Posture (..)
  , BranchRoleWire (..)
  , ProposedEditWire (..)
  , FoldDecision (..)

    -- * The gate
  , GatePolicy (..)
  , GateVerdict (..)
  , LayerApproval (..)
  , gateApplies

    -- * The seed gate ('Harness.loop'\'s opening ask)
  , SeedQuestion (..)

    -- * A window's ask for operator intent
  , OperatorSteering (..)

    -- * Wire enum to base functor (the one place they are mapped)
  , roleOf
  , postureLayer

    -- * Layer plumbing (pure, order-preserving)
  , layerBranches
  , rebuildLayer
  , indexLayer
  , layerStrategy
  , layerPosture
  , renderOrigin

    -- * What a node folds to
  , NodeAnswer (..)
  , nodeLine
  , subtreeLines

    -- * Rendering
  , render
  ) where

import Data.List.NonEmpty (NonEmpty ((:|)))
import qualified Data.List.NonEmpty as NE
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON (..), Result (..), ToJSON)
import Tidepool.Aeson.FromJSON (genericParseJSON)
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
  , -- | The companion's WORKING DRAFT (PRD 21 lane C4, "Two edit channels" —
    -- the in-heap draft C4's checked-edit path targets; file-shaped content
    -- is a separate C5 lane). Only a node's own fold can ever change it
    -- (@Harness.foldAt@), and only by running an approved 'Th.EditPlan'
    -- against the value this field held at TURN START — never by a model
    -- writing to it directly.
    draft :: Text
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
  deriving (Generic, ToJSON, Show, Eq)

-- | A negative cap is unrepresentable-in-spirit but the fields are plain
-- 'Int' (every cap-consuming call site reads one directly, with no newtype to
-- unwrap) — so the decode itself is where a negative value is refused, named
-- by field, rather than crossing the operator's JSON boundary and producing
-- accidental semantics downstream (a negative 'gateMaxRounds' making
-- @done >= gateMaxRounds@ true on round 0, silently auto-accepting a layer
-- with no presentation at all).  Structural decode first
-- ('genericParseJSON', unchanged wire shape), THEN validate — so a
-- malformed-shape error still names the right field via the ordinary
-- decode path.
instance FromJSON Config where
  parseJSON v = do
    c <- genericParseJSON v
    nonNeg "maxDepth" c.maxDepth
    nonNeg "maxNodes" c.maxNodes
    nonNeg "maxFanOut" c.maxFanOut
    nonNeg "gateMaxRounds" c.gateMaxRounds
    pure c

-- | Fail a decode, naming the field, when an Int meant to be a cap or a
-- count arrives negative.  Shared by 'Config' and 'GatePolicy', the two
-- decodable types that carry one.
nonNeg :: Text -> Int -> Result ()
nonNeg field n
  | n >= 0 = pure ()
  | otherwise = Error (unpack (field <> " must be non-negative, got " <> show n))

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

-- | The question is deliberately EMPTY: seeding it is the OPERATOR's first
-- act, not the author's — 'Harness.loop' opens by asking for it
-- (@askUser \@SeedQuestion@) whenever it is empty, before any model window
-- runs (operator decision, 2026-08-19; a hardcoded question meant the first
-- attended run spent real model turns on a question nobody chose).
initialState :: State
initialState =
  State
    { question = ""
    , config =
        Config
          { -- Deep by default (operator decision, 2026-08-20): the companion
            -- should be able to explore genuinely recursive forks rather
            -- than hitting a silent wall a few layers down. maxNodes and
            -- maxFanOut remain the real budget guards; every forced finish
            -- now names its own reason and numbers (see 'renderOrigin'), so
            -- raising this ceiling trades a legible cap for a deeper one,
            -- not for a less legible one.
            maxDepth = 6
          , -- Profligate on purpose (operator decision, 2026-08-20): at 12,
            -- a root split at full fan-out left every child an allowance of
            -- ~2 — too poor to ever split again, so trees pinned flat at
            -- depth 1 across two dogfood runs. 32 lets depth-2 exploration
            -- happen without anyone rationing.
            maxNodes = 32
          , maxFanOut = 4
          , -- Autonomy by default (operator decision, 2026-08-19): splits
            -- run without per-layer operator review. The operator hears
            -- from a window through its own 'OperatorSteering' ask when it
            -- genuinely needs intent clarified — reviewing every proposed
            -- split cost more attention than it bought. The gate machinery
            -- stays config-selectable for runs that want it.
            gatePolicy = GateOff
          , gateMaxRounds = 8
          }
    , turnCount = 0
    , lastRun = Nothing
    , draft = ""
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
-- The brief a node is prompted with
-- ---------------------------------------------------------------------------

-- | A node's own branch, rendered into its coalgebra prompt.  This is the
-- WHOLE of what a child's prompt says about where it sits; everything else it
-- knows from above it inherits as CONTEXT, by being branched off its parent's
-- frozen post-coalgebra window (@Harness.discover@), never as composed text.
renderBrief :: ForkBrief -> Text
renderBrief b =
  [fmt|Your branch: {b.title} ({show b.role})
{b.instruction}|]

-- ---------------------------------------------------------------------------
-- What a window may finalize
-- ---------------------------------------------------------------------------

-- | The COALGEBRA's answer.  ONE layer, and NO RECURSIVE ARM — that is the
-- type-level guarantee behind PRD 21 locked decision 1 ("the root is never
-- asked for descendant shape").  A coalgebra invocation is structurally
-- incapable of describing more than its own layer, so a model that tries to
-- hand back a whole tree produces a decode error, not a deeper tree.
data LayerProposal
  = -- | Renamed from @finishDraft@ (companion review DEFECT-adjacent
    -- clarity fix): this field is easily confused with 'State''s own
    -- persistent working @draft@, and it is not that — it is this ONE
    -- node's local answer, never itself written into the draft (only an
    -- approved 'ProposedEditWire' can do that).
    ProposeFinish {localAnswer :: Text}
  | ProposeSplit
      { splitPosture  :: Posture
      , -- | The focus, the decision, or the claim — per the posture.
        splitFocus    :: Text
      , -- | A plain list, not a 'NE.NonEmpty': the wire shape a model writes
        -- must be an ordinary JSON array, and emptiness is a CONDITION the
        -- driver detects (@Harness.layerFromProposal@) rather than a shape the model
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

data BranchRoleWire = Primary | Alternative | Critic
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | One proposed edit to the companion's working draft (PRD 21 lane C4).
-- PLAIN DATA on purpose, not the real @s -> Either EditFailure s@ closure
-- @Tidepool.Thought.EditPlan@ ultimately wants — both windows this harness
-- ever finalizes across (@runLLMTurnBranch@/@runLLMTurnFork@) refuse a
-- finalized answer that carries a live function
-- ("runLLMTurnBranch answer must be plain data — a closure cannot cross";
-- "a concurrent fanout\/fork answer must be plain data in this driver (v1
-- scope)"), so a window can never author the closure itself. The RUNTIME
-- (@Harness.stampProposed@\/@wrapEdit@) is what turns this plain proposal
-- into the real, id-stamped 'Tidepool.Thought.Artifact', including every
-- validation the consultation's DEFECT/VALIDATION findings ask for (blank
-- intent, blank/no-op payload, per-fold and per-payload caps — none of it
-- lives here, all of it lives at the stamp).
--
-- TWO constructors, BOTH record syntax (a payload constructor in a sum must
-- use record syntax — the vendored generic JSON has no key to put a
-- positional field under and rejects one with a compile-time @TypeError@):
--
-- * 'AppendEdit' — the pre-existing shape: append @editAppend@ to the
--   draft. The runtime closure ('Harness.wrapEdit') inserts a blank-line
--   paragraph separator before the appended block when the draft is
--   already nonempty, so sibling appends compose as paragraphs rather than
--   running together.
-- * 'ReplaceOnce' — the new edit vocabulary (companion review step 7): a
--   nonempty @editNeedle@ that must occur EXACTLY ONCE in the draft the
--   plan actually runs against — zero or multiple occurrences is a typed
--   'Tidepool.Thought.EditFailure' naming the count, never a silent
--   first-match replace. An empty @editReplacement@ is an ordinary
--   deletion, not a special case.
data ProposedEditWire
  = AppendEdit {editIntent :: Text, editAppend :: Text}
  | ReplaceOnce {editIntent :: Text, editNeedle :: Text, editReplacement :: Text}
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | The ALGEBRA's answer (PRD 21 lane C4 — @Tidepool.Thought.FoldDecision@'s
-- wire shape). @foldSynthesis@\/@foldTensions@ are unchanged from the old
-- @FoldProposal@; the rest is OPTIONAL and defaults to doing nothing, so a
-- narrative fold that never mentions them behaves exactly as before:
--
-- * @foldEditsInOrder@ collapses the old @foldSelected@\/@foldComposition@
--   pair (companion review step 5 — the wire never needed two lists that
--   always had to agree on membership) into ONE ordered list of THIS
--   NODE'S OWN CHILDREN's already-advertised artifact ids: order IS
--   application order, an empty list means none, and every id must be
--   unique (a duplicate is refused at validation — see
--   @Harness.resolveWithRetry@ — never silently deduped past the model's
--   own view of what it selected). A leaf's own fold has no children, so
--   its pool is always empty and nothing it names here can ever resolve —
--   selection can only ever approve what a CHILD actually proposed, never
--   conjure one out of thin air.
-- * @foldProposed@ is how THIS node contributes a brand-new edit of its
--   own — a leaf's ONLY route to proposing anything, since it has no
--   children to select from. A leaf's own proposals are never selectable
--   at the leaf's own fold; they only become selectable one level up, at
--   its PARENT's fold, exactly like a child's. The ROOT fold is the one
--   exception: @foldProposed@ is FORBIDDEN there (companion review step 4)
--   and every entry is refused at the stamp, journaled, and never enters
--   the pool.
data FoldDecision = FoldDecision
  { foldSynthesis    :: Text
  , foldTensions     :: [Text]
  , foldEditsInOrder :: [Text]
  , foldProposed     :: [ProposedEditWire]
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | The three wire-to-base-functor mappings, and the ONLY place the wire
-- enums a model writes meet @Tidepool.Thought@'s own constructors.  They are
-- pure and element-polymorphic, so they stay here with the wire types even
-- though their one caller (@Harness.layerFromProposal@) had to move.
roleOf :: BranchRoleWire -> BranchRole
roleOf r = case r of
  Primary -> Th.Primary
  Alternative -> Th.Alternative
  Critic -> Th.Critic

postureLayer :: Posture -> Text -> NE.NonEmpty (Th.Branch a) -> Strategy -> ThoughtF a
postureLayer po f brs s = case po of
  Explore -> Th.Explore f brs s
  Compare -> Th.Compare f brs s
  Challenge -> Th.Challenge f brs s

-- ---------------------------------------------------------------------------
-- The gate
-- ---------------------------------------------------------------------------

-- | When the operator is asked to approve a layer.
--
-- 'GateOff' is what the scripted acceptance tier runs under, and it is a real
-- configuration rather than a test hook — an unattended companion turn is a
-- legitimate mode, and under it NO suspension is raised at all.
--
-- @GateWiderThan@ carries a NAMED field for the same reason
-- 'ProposedEditWire''s payload constructors do (a payload constructor in a
-- sum must use record syntax).
data GatePolicy
  = GateOff
  | GateWiderThan {gateWidth :: Int}
  | GateEveryLayer
  deriving (Generic, ToJSON, JsonSchema, Show, Eq)

-- | A negative 'gateWidth' has no honest reading — 'gateApplies' also
-- short-circuits 'Th.Finish' unconditionally (belt AND suspenders: a
-- @GateWiderThan@ built in-Haskell rather than decoded, e.g. by a future
-- caller, still cannot make @width > n@ gate a layer with no descent to
-- approve), but refusing it here is what keeps the JSON boundary from ever
-- admitting the value at all.
instance FromJSON GatePolicy where
  parseJSON v = do
    p <- genericParseJSON v
    case p of
      GateWiderThan {gateWidth = n} -> nonNeg "gateWidth" n
      _ -> pure ()
    pure p

data GateVerdict = Approve | Prune | Amend | Add
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | The gate's form.  ONE record, so it renders as one form rather than a
-- variant chooser; the fields a verdict does not use are left empty.
-- | The operator's opening move ('Harness.loop'\'s seed gate): the question
-- the whole run investigates.  One field, so the derived form is a single
-- text input, presented BEFORE any model window runs.
data SeedQuestion = SeedQuestion
  { seedQuestion :: Text
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | A window's channel for OPERATOR intent — the steering half of the
-- autonomy default ('initialState'\'s @GateOff@ comment): windows split and
-- fold on their own, and raise THIS ask (question posted via @note@, since a
-- shape-derived form carries no prompt text of its own) only when the
-- operator's answer would genuinely change what the window does.
data OperatorSteering = OperatorSteering
  { steeringReply :: Text
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

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
-- gated — checked FIRST and unconditionally, so no policy (including a
-- 'GateWiderThan' whose width was never validated, e.g. constructed directly
-- in Haskell rather than decoded) can make @width > n@ gate a layer with no
-- descent to approve.
gateApplies :: GatePolicy -> ThoughtF a -> Bool
gateApplies policy layer = case layer of
  Th.Finish _ -> False
  _ -> case policy of
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

-- | Put a NON-EMPTY branch list back under the same posture, at whatever
-- element type the caller now holds.  TOTAL: every non-'Th.Finish' 'ThoughtF'
-- constructor already carries its branches as a 'NE.NonEmpty', so a caller
-- rebuilding one always already holds the proof of non-emptiness — carrying
-- it in the argument's type is what keeps this function total instead of
-- reaching for a partial @NE.fromList@ on a plain list a caller merely
-- claims is non-empty.
rebuildLayer :: ThoughtF a -> NE.NonEmpty (Th.Branch b) -> ThoughtF b
rebuildLayer layer brs = case layer of
  Th.Finish d -> Th.Finish d
  Th.Explore f _ s -> Th.Explore f brs s
  Th.Compare d _ s -> Th.Compare d brs s
  Th.Challenge c _ s -> Th.Challenge c brs s

-- | Attach each branch's zero-based position to its value, preserving order.
-- The descent needs the index to rebuild a child's 'NodePath', and
-- @Harness.traverseLayer@'s callback takes a bare branch — so the index rides
-- in the branch's own value rather than in a second, order-coupled list.
--
-- Matches the constructor directly rather than going through 'rebuildLayer':
-- a 'Th.Finish' has no branches to index, and every other arm's branches are
-- already the 'NE.NonEmpty' 'rebuildLayer' needs, so there is no list to
-- reconstruct and nothing partial to invoke.
indexLayer :: ThoughtF a -> ThoughtF (Int, a)
indexLayer layer = case layer of
  Th.Finish d -> Th.Finish d
  Th.Explore f bs s -> Th.Explore f (tagBranches bs) s
  Th.Compare d os s -> Th.Compare d (tagBranches os) s
  Th.Challenge c as s -> Th.Challenge c (tagBranches as) s
  where
    tagBranches = NE.zipWith tag (0 :| [1 ..])
    tag i (Th.Branch b v) = Th.Branch b (i, v)

-- | The strategy the layer CARRIES — every split now runs 'Th.Concurrent'
-- (operator decision: sibling branch windows are ALWAYS driven concurrently,
-- transparently — scheduling is never a model-visible choice, so there is no
-- separate "proposed" value to compare it against).
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

-- | Why a node finished, rendered — so a budget-forced finish and a
-- model-chosen one are distinguishable in every receipt without a second
-- journal kind. A budget-forced finish NAMES its reason and the numbers that
-- governed it ('Th.renderForcedReason' — the one rendering every caller
-- shares, so this can never disagree with the text a forced 'Draft' itself
-- carries), never a bare tag.
renderOrigin :: Draft -> Text
renderOrigin d = case d.draftOrigin of
  Th.ModelFinished -> "voluntary"
  Th.BudgetForced r -> "forced: " <> Th.renderForcedReason r
  Th.InvocationFailed f -> "failed: " <> f.failureReason

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
  , -- | THIS node's own artifact pool, offered to its PARENT's fold: its own
    -- newly-proposed artifacts (PRD 21 lane C4), stamped with ids and held
    -- live, PLUS — as of endorsement propagation (companion review step 4)
    -- — every artifact this node's own fold ENDORSED (selected via
    -- @foldEditsInOrder@) from ITS children, republished upward under
    -- their ORIGINAL id (ids are path-namespaced, so provenance survives
    -- to the root unchanged). Never one this node's own fold declined to
    -- select — an available edit a fold omits is dropped from that route
    -- permanently, not re-offered. Available for exactly this node's
    -- PARENT to select by id; a parent that does not name it drops it in
    -- turn. No 'Eq'\/'Show': an artifact carries a real closure.
    answerArtifacts :: [Th.Artifact Text]
  , -- | The renderable CONTENT behind every id in 'answerArtifacts' that is
    -- an edit (never an evidence artifact, whose own 'Th.Evidence' text
    -- already IS its full content) — companion review DEFECT 1's fix: the
    -- approving fold must see the exact text it is authorizing, not just
    -- the artifact's declared intent. Carried alongside the opaque
    -- 'Th.Artifact' closure (which cannot be rendered back into text) and
    -- propagated together with a republished endorsement, so a
    -- grandchild's edit is still fully legible at the root.
    answerArtifactRenders :: [(Th.ArtifactId, Text)]
  , -- | THIS node's own view of the companion's working draft, after
    -- running whatever THIS node's own fold approved against the draft as
    -- it stood at TURN START (@Harness.loop@'s @st.draft@, frozen and
    -- shared by every node — never threaded bottom-up between siblings).
    -- Only the ROOT's own value here ever becomes the next turn's
    -- persisted 'draft' and is the one application receipts call
    -- "applied"\/"persisted"; every other node's is a PREVIEW against the
    -- same frozen snapshot, read back only for its own receipt (companion
    -- review step 4).
    answerDraft     :: Text
  , -- | The branch name of the worktree THIS node ends up owning after its
    -- own merge fold (PRD 21 C5, "Worktree coordination"), if it ever
    -- acquired one -- 'Nothing' for a purely deliberative node that never
    -- needed one.  Two acquisition routes, both landing here uniformly:
    -- @Harness.mergeFold@ creating a fresh worktree and merging
    -- content-bearing children into it, OR this node's OWN coalgebra
    -- window delegating (@Tidepool.Agent.Delegate.delegate@) and the
    -- delegated cycle's bound worktree riding straight through with no
    -- wrapping worktree of its own -- @Harness.foldAt@ populates this
    -- field from a RUNTIME-STAMPED record the driver keeps (never from
    -- anything a model finalizes; see @Harness.foldAt@'s own doc on
    -- @takeDelegatedBranches@), so a model cannot fabricate or influence
    -- which branch a node hands up.  Deliberately a plain 'Text', not a
    -- live 'WorktreeHandle': that type only NAMES anything in a row
    -- containing @Worktree@, and this module has to stay compilable in
    -- the answerer's narrow row, which does not (see the module docs
    -- above on why 'NodeSeed' carries its 'ContextRef' in "Harness"
    -- instead -- the same constraint, applied here).  The branch name alone
    -- is also all a merge into a PARENT's worktree ever needs: every
    -- managed worktree shares one repository's ref namespace, so a branch
    -- is mergeable by name from any worktree of it, with no handle to carry.
    answerMergeBranch :: Maybe Text
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
{budgetLine}{draftBlock}

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
{budgetLine}{draftBlock}
Turns folded: {show st.turnCount}|]
  where
    c = st.config
    budgetLine :: Text
    budgetLine =
      [fmt|Budget: depth {c.maxDepth}, {c.maxNodes} nodes, fan-out {c.maxFanOut}; gate {renderPolicy c.gatePolicy} (at most {c.gateMaxRounds} rounds)|]
    -- Shown only once a fold has actually changed the draft (PRD 21 lane
    -- C4) — an empty draft is exactly today's pre-C4 behavior, and this
    -- stays silent about it rather than announcing an empty string. A
    -- delimited BLOCK, not an inline "Draft: ..." line (companion review
    -- step 8d) — the same reason 'Harness.algebraPrompt' renders the
    -- turn-start draft as a block: a multi-line draft must not be
    -- ambiguous with whatever text follows it.
    draftBlock :: Text
    draftBlock
      | st.draft == "" = ""
      | otherwise =
          [fmt|
--- BEGIN DRAFT ---
{st.draft}
--- END DRAFT ---|]
    tensionsBlock r = case r.runTensions of
      [] -> "" :: Text
      ts -> "\nTensions:\n" <> T.intercalate "\n" (map ("- " <>) ts) <> "\n"
    -- Gate interventions are NOT counted here.  They happen in a node's
    -- coalgebra, and an algebra never sees its own seed (@ThoughtF@ has no
    -- task slot), so with no state effect in the outer row there is no
    -- channel that carries the count into the fold.  The journal has every
    -- one of them, keyed by node path.
    receiptLine r =
      [fmt|receipt: {show r.runNodes} nodes, {show r.runWindows} windows, {show r.runForced} budget-forced finishes, {show r.runFailed} failures; gate interventions are journaled per node under kind "gate"|]

renderPolicy :: GatePolicy -> Text
renderPolicy p = case p of
  GateOff -> "off"
  GateEveryLayer -> "every layer"
  GateWiderThan {gateWidth = n} -> [fmt|layers wider than {show n}|]
