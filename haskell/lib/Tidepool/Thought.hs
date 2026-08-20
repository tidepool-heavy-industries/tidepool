{-# LANGUAGE DeriveFunctor #-}
{-# LANGUAGE DeriveFoldable #-}
{-# LANGUAGE DeriveTraversable #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The recursive companion's pure semantic fixture.
--
-- 'ThoughtF' is the companion's base functor: a node either finishes locally
-- or defines only its own next layer of branches — a branch carries its own
-- 'ForkBrief' and 'BranchRole', which a bare kids list would lose, so
-- 'thoughtHylo' is 'Tidepool.Swarm.hyloM's one-recursive-call-site shape
-- specialized to 'ThoughtF''s own derived 'Traversable' rather than
-- 'Tidepool.Swarm.PlanF''s task-plus-flat-kids shape — not a second
-- recursion engine; the body is the identical one-liner.
--
-- Budget clamping ('depthCapped', 'nodeCapped', 'fanOutCapped') mirrors
-- 'Tidepool.Swarm.capped'\/'budgeted'\/'gated': each forces a local 'Finish'
-- rather than letting the wrapped coalgebra run past its cap, stamped on the
-- result ('FinishOrigin' inside 'Draft') rather than materialized as a plan
-- the driver consults separately.
--
-- Genuine model-invocation failure is represented the same way: a coalgebra
-- that cannot decide a real layer still returns an ordinary 'Finish', tagged
-- 'InvocationFailed'.  A caller's algebra reads both budget-forced and
-- invocation-failed 'Finish' nodes as data through the same exhaustive match
-- it uses for everything else — nothing in this module throws.
module Tidepool.Thought
  ( -- * The base functor
    ThoughtF (..)
  , Branch (..)
  , ForkBrief (..)
  , BranchRole (..)
  , Strategy (..)

    -- * Budgets and forced finish
  , Budget (..)
  , ForcedReason (..)
  , FinishOrigin (..)
  , Draft (..)
  , branchCount

    -- * What crosses the model boundary vs. what the runtime stamps
  , ArtifactId (..)
  , EditIntent (..)
  , Evidence (..)
  , Artifact (..)
  , artifactId
  , ProposedArtifact (..)
  , ModelContribution (..)
  , NodeFailure (..)
  , NodeReceipt (..)
  , NodeResult (..)
  , CompositionOrder (..)
  , FoldDecision (..)

    -- * C4 — checked edits: the authority-bearing sum, the consuming
    -- capability, and the runtime apply path (PRD 21 open question 4)
  , EditFailure (..)
  , EditPlan (..)
  , artifactEditPlan
  , FoldProduct (..)
  , resolveSelection
  , ApprovedEdits
  , approve
  , approvedOrder
  , EditReceipt (..)
  , applyEdits

    -- * C4 edit vocabulary (companion review step 7) — the two edit
    -- closures' own pure behavior, shared between the recursive
    -- companion's runtime ("Harness.hs"'s @wrapEdit@) and this module's own
    -- property tests
  , appendWithSeparator
  , replaceExactlyOnce

    -- * The driver
  , Coalg
  , Alg
  , thoughtHylo
  , GroupCoalg
  , thoughtHyloGrouped

    -- * Budget middleware
  , depthCapped
  , nodeCapped
  , fanOutCapped
  ) where

import Control.Monad.State.Class (MonadState, get, put)
import Data.Foldable (toList)
import Data.List (find, nub)
import Data.List.NonEmpty (NonEmpty (..))
import qualified Data.List.NonEmpty as NE
import Data.Text (Text)
import qualified Data.Text as T

-- ---------------------------------------------------------------------------
-- The base functor (PRD 21, "Core types")
-- ---------------------------------------------------------------------------

-- | One node's discovered layer: finish locally, or split into the next
-- layer's branches under one of three postures. Describes ONE layer only —
-- a branch's own value is an opaque @a@ (an undiscovered seed, or later a
-- worked result), never an already-unfolded subtree, so a single coalgebra
-- invocation is structurally incapable of deciding more than its own layer.
data ThoughtF a
  = Finish {draft :: Draft}
  | Explore {focus :: Text, branches :: NonEmpty (Branch a), strategy :: Strategy}
  | Compare {decision :: Text, options :: NonEmpty (Branch a), strategy :: Strategy}
  | Challenge {claim :: Text, attacks :: NonEmpty (Branch a), strategy :: Strategy}
  deriving (Eq, Show, Functor, Foldable, Traversable)

data Branch a = Branch {brief :: ForkBrief, value :: a}
  deriving (Eq, Show, Functor, Foldable, Traversable)

data ForkBrief = ForkBrief {title :: Text, role :: BranchRole, instruction :: Text}
  deriving (Eq, Show)

-- | Placeholder enumeration (PRD: types are starting points, "expected to be
-- edited through dogfood") — what stance a branch takes relative to its
-- parent's posture.
data BranchRole = Primary | Alternative | Critic
  deriving (Eq, Show)

-- | Controls scheduling only, never context inheritance (PRD 21) — every
-- child forks the same frozen snapshot regardless of 'Strategy'. Not
-- interpreted anywhere in this pure fixture: residency/concurrency is
-- existing substrate (green threads), out of C0's scope.
data Strategy = Sequential | Concurrent | Pooled Int
  deriving (Eq, Show)

-- ---------------------------------------------------------------------------
-- Budgets and forced finish
-- ---------------------------------------------------------------------------

-- | Depth, node-count, and fan-out caps (PRD locked decision 9). Rounds,
-- deadline, and cost are deferred — this pure fixture implements exactly the
-- three caps the lane spec asks for; nothing here forecloses adding the
-- rest later.
data Budget = Budget {maxDepth :: Int, maxNodes :: Int, maxFanOut :: Int}
  deriving (Eq, Show)

-- | Which cap forced a local finish.
data ForcedReason = ForcedDepth | ForcedNodeCount | ForcedFanOut
  deriving (Eq, Show)

-- | Why a node carries 'Finish': the model chose to stop, a budget forced it
-- (still an ordinary completion — PRD: "a model-proposed Strategy is
-- transformed EXPLICITLY... and the transformation stamped in the
-- receipt"), or the coalgebra invocation itself exited abnormally (a
-- genuine failure). Distinguishing these is what lets one caller algebra
-- fold both budget caps and invocation failure as ordinary data without
-- conflating "finished early, on purpose or by policy" with "never produced
-- a real answer".
data FinishOrigin
  = ModelFinished
  | BudgetForced ForcedReason
  | InvocationFailed NodeFailure
  deriving (Eq, Show)

-- | Placeholder for the PRD's opaque @Draft@ payload type, extended with
-- 'FinishOrigin' (why this node finished) and 'draftDepth' (this node's own
-- depth). The depth field exists because an algebra folding a bare
-- 'ThoughtF' layer can recover an interior node's depth from any child
-- (child depth minus one), but a childless 'Finish' has no child to read it
-- from — so the one node shape with no children is the one place depth must
-- ride on the data itself.
data Draft = Draft {draftText :: Text, draftOrigin :: FinishOrigin, draftDepth :: Int}
  deriving (Eq, Show)

-- | How many branches a layer declared — 0 for 'Finish'.
branchCount :: ThoughtF a -> Int
branchCount layer = case layer of
  Finish {} -> 0
  Explore {branches = bs} -> NE.length bs
  Compare {options = os} -> NE.length os
  Challenge {attacks = as} -> NE.length as

-- ---------------------------------------------------------------------------
-- What crosses the model boundary vs. what the runtime stamps
--
-- PRD's dataflow principle, locked: the model attests only to
-- 'ModelContribution' (no ids, no receipts — it cannot attest runtime
-- facts); the RUNTIME assigns ids, computes previews, and stamps the
-- receipt around it, producing 'NodeResult'. Keeping these as two distinct
-- types (rather than one record with optional runtime fields) makes "the
-- model cannot attest to its own execution" a type-level fact, not a
-- convention.
-- ---------------------------------------------------------------------------

newtype ArtifactId = ArtifactId Text
  deriving (Eq, Ord, Show)

-- | Placeholder for the PRD's opaque @EditIntent@ payload.
newtype EditIntent = EditIntent Text
  deriving (Eq, Show)

-- | Placeholder for the PRD's opaque @Evidence@ payload.
newtype Evidence = Evidence Text
  deriving (Eq, Show)

-- | An id-stamped artifact the runtime holds live (PRD: "held live by the
-- runtime"). No 'Eq' instance: a closure has no meaningful equality, only
-- its id and intent do — callers compare on those, never on the whole
-- value.
data Artifact s
  = EditArtifact ArtifactId EditIntent (s -> Either EditFailure s)
  | EvidenceArtifact ArtifactId Evidence

-- | An artifact's id, regardless of which constructor it is.
artifactId :: Artifact s -> ArtifactId
artifactId a = case a of
  EditArtifact aid _ _ -> aid
  EvidenceArtifact aid _ -> aid

-- | An unstamped artifact proposal, before the runtime assigns it an id.
data ProposedArtifact s = ProposedArtifact
  { proposedIntent :: EditIntent
  , -- | 'Nothing' for an evidence-only proposal.
    proposedApply :: Maybe (s -> Either EditFailure s)
  }

-- | What the MODEL finalizes (PRD): a rendered view of this node plus
-- whatever it proposes — no ids, no receipts.
data ModelContribution s = ModelContribution
  { contributionView :: Text
  , contributionProposed :: [ProposedArtifact s]
  }

newtype NodeFailure = NodeFailure {failureReason :: Text}
  deriving (Eq, Show)

-- | What the runtime observed about one fold. 'receiptForced' is set exactly
-- when a budget cap produced this node's 'Finish' rather than the model
-- choosing to stop (PRD: "the transformation stamped in the receipt").
data NodeReceipt = NodeReceipt {receiptDepth :: Int, receiptForced :: Maybe ForcedReason}
  deriving (Eq, Show)

-- | What the RUNTIME constructs around a 'ModelContribution' — ids assigned,
-- receipt stamped, and failure representable (PRD locked decision 6): a
-- coalgebra or algebra invocation that exits abnormally is folded as
-- 'NodeFailed' at its branch position, never an exception that erases
-- sibling results.
data NodeResult s
  = NodeSucceeded
      { contribution :: ModelContribution s
      , artifacts :: [Artifact s]
      , receipt :: NodeReceipt
      }
  | NodeFailed
      { failure :: NodeFailure
      , receipt :: NodeReceipt
      }

-- | Placeholder for the PRD's opaque @CompositionOrder@ payload: the order
-- the algebra composes selected artifacts in.
newtype CompositionOrder = CompositionOrder [ArtifactId]
  deriving (Eq, Show)

-- | What the algebra's model window decides about a realized layer: a
-- synthesis, which child (or self-authored) artifacts to keep, in what
-- order, plus any new proposals of its own.
data FoldDecision s = FoldDecision
  { synthesis :: Text
  , selected :: [ArtifactId]
  , composition :: CompositionOrder
  , decisionProposed :: [ProposedArtifact s]
  }

-- ---------------------------------------------------------------------------
-- C4 — checked edits (PRD 21 open question 4)
--
-- The authority-bearing stage is a DISTINCT sum, not a flag or an
-- optionally-empty list: 'FoldProduct' either carries no artifacts at all
-- ('Narrative') or a nonempty, already-resolved list of them
-- ('ProposedEdits'). 'ApprovedEdits' is the consuming capability that
-- 'applyEdits' requires, and its constructor is NOT exported — the only way
-- to obtain one is 'approve', and 'approve' can only ever succeed on a
-- 'FoldProduct' that is already 'ProposedEdits'. A 'Narrative' answer is
-- text; it carries no 'EditPlan' anywhere in its structure, so there is
-- nothing in it 'approve' could resolve into a capability. This is what
-- "unrepresentable, not runtime-checked" means here: 'applyEdits' takes an
-- 'ApprovedEdits' by VALUE, and no caller anywhere in this module or outside
-- it can manufacture one except through 'approve'.
-- ---------------------------------------------------------------------------

-- | Why an edit closure refused to apply — the artifact's OWN invariant
-- check (PRD locked decision 7 / C4: "the artifact closure's own Either
-- EditFailure s"), never a build or typecheck gate, because v1 edit targets
-- are markdown/state, not code. Distinct from 'NodeFailure', which is a
-- coalgebra\/algebra INVOCATION failing outright — an 'EditFailure' means the
-- invocation succeeded and the edit it proposed was refused at apply time.
newtype EditFailure = EditFailure {editFailureReason :: Text}
  deriving (Eq, Show)

-- | The view of an 'Artifact' that actually carries authority to mutate: an
-- id, its intent, and the closure. 'EvidenceArtifact's have none of the
-- latter, so they can never become one — see 'artifactEditPlan'.
data EditPlan s = EditPlan
  { editPlanId :: ArtifactId
  , editPlanIntent :: EditIntent
  , editPlanApply :: s -> Either EditFailure s
  }

-- | 'Nothing' for an 'EvidenceArtifact' — it carries no closure, so it is
-- structurally incapable of becoming an 'EditPlan'.
artifactEditPlan :: Artifact s -> Maybe (EditPlan s)
artifactEditPlan a = case a of
  EditArtifact aid intent f -> Just (EditPlan aid intent f)
  EvidenceArtifact _ _ -> Nothing

-- | What a fold's authority-bearing stage actually is: free text with no
-- power to mutate anything, or a nonempty, already-ordered list of REAL,
-- resolved edit plans. Never both, never neither-but-claims-one: emptiness
-- is not a value this type can hold under 'ProposedEdits', so "the
-- selection resolved to nothing" and "the algebra wrote prose" are the one
-- honest answer ('resolveSelection' below), not two states a caller must
-- keep in sync by hand.
data FoldProduct s
  = Narrative Text
  | ProposedEdits (NonEmpty (EditPlan s))

-- | Resolve a 'FoldDecision''s id selection against the artifacts actually
-- on hand, in the decision's own 'CompositionOrder'. An id is kept only when
-- it is BOTH named in 'selected' AND present in the pool as an
-- edit-bearing artifact — an id naming nothing resolvable (a typo, an
-- 'EvidenceArtifact', an id from a sibling's pool) is silently dropped:
-- selection can shrink what runs, never conjure an edit that was never
-- proposed. If nothing survives resolution, the result is 'Narrative' —
-- there is no way to construct an empty 'ProposedEdits'.
--
-- 'CompositionOrder' is DEDUPED, stably, before resolution: a caller that
-- feeds a model-authored order straight through (as
-- @Harness.resolveAndApply@ does) cannot assume the model wrote each id at
-- most once, and the composition's own order — not 'selected''s, and
-- definitely not the pool's declaration order — is what decides which
-- occurrence survives. 'nub' keeps the FIRST occurrence and drops the rest,
-- which is exactly "first position wins": a duplicated id resolves (and
-- applies) at most once.
resolveSelection :: FoldDecision s -> [Artifact s] -> FoldProduct s
resolveSelection decision pool = case NE.nonEmpty resolved of
  Nothing -> Narrative (synthesis decision)
  Just plans -> ProposedEdits plans
  where
    CompositionOrder orderedIds = composition decision
    dedupedIds = nub orderedIds
    resolved =
      [ plan
      | aid <- dedupedIds
      , aid `elem` selected decision
      , Just artifact <- [find ((== aid) . artifactId) pool]
      , Just plan <- [artifactEditPlan artifact]
      ]

-- | The consuming capability PRD 21 open question 4 asks for. Its
-- constructor is deliberately NOT exported — see the section header above.
newtype ApprovedEdits s = ApprovedEdits (NonEmpty (EditPlan s))

-- | The ONLY route to an 'ApprovedEdits'. 'Nothing' on 'Narrative' — always,
-- structurally, because 'Narrative' carries no 'EditPlan' to approve.
approve :: FoldProduct s -> Maybe (ApprovedEdits s)
approve fp = case fp of
  Narrative _ -> Nothing
  ProposedEdits plans -> Just (ApprovedEdits plans)

-- | The order 'applyEdits' actually runs in, read back off an already-minted
-- capability rather than re-derived from the 'FoldDecision' that produced
-- it — a receipt built from this can never claim an order 'resolveSelection'
-- did not actually resolve to.
approvedOrder :: ApprovedEdits s -> CompositionOrder
approvedOrder (ApprovedEdits plans) = CompositionOrder (NE.toList (fmap editPlanId plans))

-- | What the runtime observed about one applied (or refused) edit.
-- 'receiptBefore' is always the preview of the state the plan actually SAW;
-- 'receiptOutcome' is 'Left' the plan's own refusal or 'Right' the preview
-- of the state it produced.
data EditReceipt = EditReceipt
  { receiptArtifact :: ArtifactId
  , receiptIntent :: EditIntent
  , receiptBefore :: Text
  , receiptOutcome :: Either EditFailure Text
  }
  deriving (Eq, Show)

-- | Apply every approved edit, in the capability's own order, to a known
-- starting snapshot. Each plan runs against the state the PRECEDING
-- SUCCESSFUL plan left behind: a refused plan leaves the snapshot
-- unchanged, so the next plan in line still sees the last good state rather
-- than one poisoned by a failure that never actually took effect. This is
-- PRD locked decision 6's shape, read onto composition — one failing
-- artifact never erases, or corrupts the input to, any sibling — and it is
-- why the result carries exactly one receipt per plan, always: a failure
-- is data at its own position, never an early return that drops the rest.
applyEdits :: (s -> Text) -> ApprovedEdits s -> s -> (s, NonEmpty EditReceipt)
applyEdits preview (ApprovedEdits plans) snapshot0 = go snapshot0 plans
  where
    go s (p :| rest) =
      let before = preview s
          (s', outcome) = case editPlanApply p s of
            Left failure -> (s, Left failure)
            Right s2 -> (s2, Right (preview s2))
          thisReceipt = EditReceipt (editPlanId p) (editPlanIntent p) before outcome
       in case NE.nonEmpty rest of
            Nothing -> (s', thisReceipt :| [])
            Just rest' -> let (sFinal, receipts) = go s' rest' in (sFinal, thisReceipt `NE.cons` receipts)

-- ---------------------------------------------------------------------------
-- C4 edit vocabulary (companion review step 7) — the two edit closures'
-- own pure behavior. Neither is model-facing or wire-shaped (that is
-- "HarnessTypes.hs"'s @ProposedEditWire@); these are what the RUNTIME'S
-- closure over one actually does, kept here — pure and total — so they
-- share ONE implementation with this module's own QuickCheck properties
-- rather than living only inside "Harness.hs"'s @wrapEdit@, untestable by
-- anything but the scripted Rust acceptance tier.
-- ---------------------------------------------------------------------------

-- | An 'AppendEdit''s own closure body: append 'add' to 'draft', inserting a
-- blank-line paragraph separator first when 'draft' is already nonempty —
-- so sibling appends compose as paragraphs rather than running together.
appendWithSeparator :: Text -> Text -> Text
appendWithSeparator draft add
  | T.null draft = add
  | otherwise = draft <> "\n\n" <> add

-- | A 'ReplaceOnce''s own closure body: replace the ONE occurrence of
-- 'needle' in 'haystack' with 'replacement'. Zero or multiple occurrences
-- is a typed 'EditFailure' naming the count, never a silent first-match
-- replace. An empty 'replacement' is an ordinary deletion, not a special
-- case. A blank 'needle' is refused outright — 'ProposedEditWire''s own
-- wire-level validation (\"HarnessTypes.hs\") already keeps one from ever
-- reaching this far, but the function stays total rather than partial on
-- an input its own type does not rule out.
replaceExactlyOnce :: Text -> Text -> Text -> Either EditFailure Text
replaceExactlyOnce needle replacement haystack
  | T.null needle = Left (EditFailure "needle must be nonempty")
  | otherwise = case T.count needle haystack of
      1 -> Right (before <> replacement <> T.drop (T.length needle) rest)
      n -> Left (EditFailure ("needle must occur exactly once in the draft, occurs " <> tshow n <> " times"))
  where
    (before, rest) = T.breakOn needle haystack
    tshow = T.pack . show

-- ---------------------------------------------------------------------------
-- The driver
-- ---------------------------------------------------------------------------

-- | How to split: a seed becomes its next 'ThoughtF' layer.
type Coalg m a = a -> m (ThoughtF a)

-- | How to combine: a realized layer (branch seeds already replaced by their
-- worked results, in declared branch order — never completion order, PRD
-- locked decision 5) folds into one value.
type Alg m b = ThoughtF b -> m b

-- | The one recursive call site (mirrors 'Tidepool.Swarm.hyloM' exactly):
-- unfold with the coalgebra, recurse into every branch via 'ThoughtF''s own
-- derived 'Traversable' (which preserves declared order by construction —
-- that IS the completion-order guarantee, not something this function has
-- to enforce separately), then fold with the algebra. No 'ThoughtF' tree
-- ever materializes beyond the current layer.
thoughtHylo :: Monad m => Alg m b -> Coalg m a -> a -> m b
thoughtHylo alg coalg = go
  where
    go a = coalg a >>= traverse go >>= alg

-- | How to split a whole SIBLING GROUP at once: every seed in the group
-- shares one parent, and a 'GroupCoalg' resolves all of them together — the
-- one caller-visible seam that lets a caller batch its OWN suspending work
-- (one bulk round trip instead of N sequential ones) instead of windowing
-- siblings one at a time. Answers are returned in the SAME order the seeds
-- were given, never completion order — the same guarantee 'Coalg' already
-- gives per node, extended across a group.
type GroupCoalg m a = NonEmpty a -> m (NonEmpty (ThoughtF a))

-- | Level-synchronized sibling of 'thoughtHylo' (operator decision: sibling
-- branch windows are ALWAYS driven concurrently, transparently — scheduling
-- is never a model-visible choice). 'thoughtHylo''s own @coalg a >>= traverse
-- go >>= alg@ cannot express this: ordinary monadic 'traverse' runs each
-- sibling's WHOLE subtree to completion (via the recursive @go@) before its
-- neighbour's own coalgebra call ever happens, so a caller wanting siblings
-- windowed TOGETHER cannot get there by tweaking 'Coalg' alone — it has to
-- unfold a GROUP at once. This is that: unfold @seed@'s OWN (singleton)
-- group first, then for every subsequent layer, unfold ITS children — one
-- more sibling group, sharing one parent — together, recursing into each
-- child's own group independently once its layer is known. Still 'ThoughtF'
-- one-layer-at-a-time discipline, and still 'ThoughtF''s own derived
-- 'Traversable' order (declared order, never completion order) both within
-- a group and across the fold; only the sibling GROUPING of the unfold
-- itself is new. Not a second recursion engine grafted onto 'thoughtHylo':
-- 'thoughtHylo' is untouched, byte for byte, and every existing caller of it
-- keeps its current one-seed-at-a-time semantics.
thoughtHyloGrouped :: Monad m => Alg m b -> GroupCoalg m a -> a -> m b
thoughtHyloGrouped alg gcoalg seed0 = do
  layer0 <- soleLayer <$> gcoalg (seed0 :| [])
  goLayer layer0
  where
    soleLayer (l :| _) = l
    goLayer layer = case NE.nonEmpty (toList layer) of
      Nothing -> alg (retagEmpty layer)
      Just seeds -> do
        childLayers <- gcoalg seeds
        foldedChildren <- traverse goLayer childLayers
        alg (reattach foldedChildren layer)

-- | A layer with NO branch seeds is structurally always 'Finish' (every
-- other constructor's branches are a 'NonEmpty', so it always has at least
-- one) — this is 'fmap' over that empty structure, total because there is
-- nothing of type @a@ inside to convert.
retagEmpty :: ThoughtF a -> ThoughtF b
retagEmpty layer = case layer of
  Finish d -> Finish d
  _ -> error "Tidepool.Thought.retagEmpty: a layer with branches is not childless"

-- | Replace @layer@'s own branch seeds with @bs@, one for one, in order —
-- the 'GroupCoalg'-batched sibling of 'traverse''s ordinary per-node
-- rebuild. @bs@ is ALWAYS the same length as @layer@'s own branch list by
-- construction (built from it, one call site above), so 'NE.zipWith' never
-- truncates.
reattach :: NonEmpty b -> ThoughtF a -> ThoughtF b
reattach bs layer = case layer of
  Finish d -> Finish d
  Explore f brs s -> Explore f (NE.zipWith carry brs bs) s
  Compare d brs s -> Compare d (NE.zipWith carry brs bs) s
  Challenge c brs s -> Challenge c (NE.zipWith carry brs bs) s
  where
    carry br b = br {value = b}

-- ---------------------------------------------------------------------------
-- Budget middleware — Coalg -> Coalg, mirroring Tidepool.Swarm's
-- capped/budgeted/gated (PRD locked decision 9: budgets clamp
-- deterministically, and a hard cap forces a local finish, stamped rather
-- than silently substituted).
-- ---------------------------------------------------------------------------

-- | Refuse to unfold past a depth, mirroring 'Tidepool.Swarm.capped': the
-- caller reads the seed's own depth (this fixture threads depth on the
-- seed, as 'Harness.hs' does for its own tree), and the wrapped coalgebra
-- never runs once the cap is reached — the cap is never itself the reason a
-- cycle gets spent.
depthCapped :: Monad m => (a -> Int) -> Int -> Coalg m a -> Coalg m a
depthCapped depthOf limit coalg a
  | depthOf a >= limit = pure (Finish (Draft "<depth cap reached>" (BudgetForced ForcedDepth) (depthOf a)))
  | otherwise = coalg a

-- | Refuse to unfold past a running node-count budget, mirroring
-- 'Tidepool.Swarm.budgeted' but over shared state rather than a per-seed
-- reader — node count is a property of the WHOLE traversal, not of any one
-- seed. Every node that is allowed to proceed spends one unit, counting the
-- root. Needs the seed's own depth too, purely to stamp the forced 'Draft'
-- correctly (see 'Draft''s doc).
nodeCapped :: MonadState Int m => (a -> Int) -> Int -> Coalg m a -> Coalg m a
nodeCapped depthOf limit coalg a = do
  n <- get
  if n >= limit
    then pure (Finish (Draft "<node-count cap reached>" (BudgetForced ForcedNodeCount) (depthOf a)))
    else put (n + 1) >> coalg a

-- | Refuse a layer whose fan-out exceeds the cap, mirroring
-- 'Tidepool.Swarm.gated': the unfold has already happened when this
-- decides, because fan-out is a property of the produced layer, not the
-- seed — refusing here refuses the DESCENT, not the coalgebra's own work.
fanOutCapped :: Monad m => (a -> Int) -> Int -> Coalg m a -> Coalg m a
fanOutCapped depthOf limit coalg a =
  coalg a >>= \layer ->
    pure
      ( if branchCount layer > limit
          then Finish (Draft "<fan-out cap reached>" (BudgetForced ForcedFanOut) (depthOf a))
          else layer
      )
