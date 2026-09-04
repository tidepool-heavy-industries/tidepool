{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, UndecidableInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, DeriveGeneric, DeriveAnyClass, QuasiQuotes, DuplicateRecordFields, OverloadedRecordDot #-}

-- | Managed git worktrees.
--
-- A resident allocates isolated worktrees, looks retained ones back up by
-- durable id, and reads what git actually became.  There is no @rebaseOnto@,
-- @cherryPick@, conflict RESOLUTION, or branch promotion here, and their
-- absence is the design rather than a gap: coding agents do that work with
-- their native tools (@gitIn@ — a thin per-harness wrapper over
-- 'Tidepool.Shell.runInTry', NOT defined here; see the note below on why),
-- and Tidepool observes the result through 'Tidepool.Event'.
--
-- 'mergeBranchInto' is the ONE deliberate exception (the recursive
-- companion's worktree-coordination fold): merge one branch into a target worktree,
-- typed and classified once — conflict vs. a git failure that never entered a
-- merge at all — instead of every authored harness re-deriving that
-- classification over raw 'gitIn'. It is not a general workflow surface;
-- resolving a conflict it reports is still authored policy.
--
-- == The vocabulary
--
-- Build a spec, create from it, get a handle:
--
-- @
-- created <- 'createWorktree' ('fromCurrentRepository' "dev-tree\/root")
-- case created of
--   Left ('SourceDirty' summary) -> ...
--   Right tree                   -> ...
-- @
--
-- A dirty source is refused by default.  The escape hatch is spelled at the
-- call site, so a reader of the resident can see that a snapshot was taken:
--
-- @
-- 'createWorktree' ('allowDirtySnapshot' ('fromCurrentRepository' "dev-tree\/root"))
-- @
--
-- == Retention
--
-- Managed worktrees are retained indefinitely in v1.  There is deliberately no
-- @releaseWorktree@ or @deleteWorktree@: losing work is worse than
-- accumulating it, and every tree, branch, snapshot ref, and receipt survives
-- restart under its 'WorktreeId'.  A tree a human removed by hand comes back
-- as 'WorktreeLost' and is never silently recreated.
--
-- == Isolation
--
-- One worktree per agent, every agent isolated.  Binding a second agent to a
-- bound worktree fails explicitly.  A reviewer is isolated like everyone else
-- — give it its own worktree created 'fromWorktree' off the branch it is
-- reviewing.
--
-- == Reading @HEAD@ across a loop-iteration boundary
--
-- 'worktreeHead' is a FRESH git read of a worktree's current @HEAD@ — not the
-- handle's recorded @sourceHead@, and not the event monitor's last-observed
-- baseline.  It is how a resident that spans loop iterations closes a gap the event
-- system deliberately will not close for it.
--
-- A subscription never replays and lives only for its loop iteration, so @HEAD@ can move
-- after one loop iteration unregisters and before the next registers.  A resident
-- reconciles that gap itself, in ordinary code, as the FIRST ACTION inside
-- the newly registered handler scope — registration is active before the read,
-- so a movement before registration is found by the reconciliation read while
-- a movement after it is queued for the handler (deduplicate by observed head
-- if both paths see the same movement).  Reading @HEAD@ before registering
-- instead reopens the very gap this closes:
--
-- @
-- 'Tidepool.Event.withHandler' ('Tidepool.Event.headChanged' tree) onChange $ do
--   current <- 'worktreeHead' tree
--   when (current \/= checkpointedHead) (reactToMissedMovement current)
--   ...
-- @
--
-- This REINFORCES no-replay rather than working around it.  The journal stays
-- diagnostic instead of quietly becoming a callback-replay mechanism, because
-- the resident — which knows what it already acted on — decides what the gap
-- meant, rather than the runtime guessing on its behalf.
--
-- HOLD: @workspaceOf :: WorktreeHandle -> Workspace@ and the coupled-spawn
-- signature are NOT exported yet — @Workspace@'s shape is still being settled
-- jointly with the agent lane, and exporting a conversion into it now would
-- freeze the wrong half of a two-sided seam.  The binding enforcement those
-- signatures rest on exists today (one worktree, one agent, explicit refusal)
-- and is tested against a scripted writer.
module Tidepool.Worktree
  ( -- * Specs
    WorktreeSpec
  , fromCurrentRepository
  , fromRef
  , fromWorktree
  , allowDirtySnapshot

    -- * Creation and lookup
  , createWorktree
  , lookupWorktree
  , listWorktrees

    -- * Handles
  , WorktreeHandle
  , WorktreeId (..)
  , BranchName
  , mkBranchName
  , GitRef
  , InProgressKind (..)
  , GitOid
  , worktreeId
  , worktreeBranch
  , worktreeHead
  , observeSubmission
  , withWorktree

    -- * Merging (the one narrow, deliberate workflow primitive)
  , MergeOutcome (..)
  , mergeBranchInto

    -- * Receipts and failures
  , WorktreeReceipt (..)
  , WorktreeSummary (..)
  , WorktreeError (..)
  , DirtySummary (..)
  , HeadState (..)
  , WorkingState (..)
  , SubmissionObservation (..)
  , renderWorktreeError
  , renderWorktreeId
  , renderBranchName
  , renderGitOid
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Proxy (Proxy (..))
import qualified Tidepool.Data.Text as T
import Tidepool.Actor.Internal (ActorDefinition, withLaunchWorktree)
import Tidepool.Aeson (FromJSON (..), Result (..), ToJSON (..), object, withObject, withText, (.:), (.:?), (.=))
import Tidepool.Aeson.Schema (JsonSchema (..), objectSchema)
import Tidepool.Aeson.Value (Value (..))
import Tidepool.Effects
  ( BranchName (..)
  , DirtyPolicy (..)
  , DirtySummary (..)
  , GitFailureReceipt (..)
  , GitOid (..)
  , GitRef
  , InProgressKind (..)
  , MergeOutcome (..)
  , HeadState (..)
  , WorkingState (..)
  , SubmissionObservation (..)
  , Worktree (WorktreeBranchOf, WorktreeHeadOf)
  , WorktreeError (..)
  , WorktreeHandle
  , WorktreeId (..)
  , WorktreeReceipt (..)
  , WorktreeSource (..)
  , WorktreeSpec (..)
  , WorktreeSummary (..)
  , createWorktree
  , liftEither
  , listWorktrees
  , lookupWorktree
  , mergeBranchInto
  , observeSubmission
  , worktreeId
  )
import Tidepool.Prelude hiding (error)

default (Int, Double, Text)

-- Generated Worktree values predate the model-facing output-schema layer.
-- These instances keep CandidateReceipt on the canonical Worktree vocabulary
-- instead of introducing a parallel JSON-shaped repository model.
instance JsonSchema WorktreeId where
  jsonSchema _ = object [("type", String "string")]

instance FromJSON WorktreeId where
  parseJSON value = WorktreeId <$> parseJSON value

instance JsonSchema BranchName where
  jsonSchema _ = object [("type", String "string")]

instance FromJSON BranchName where
  parseJSON value = BranchName <$> parseJSON value

instance JsonSchema GitOid where
  jsonSchema _ = object [("type", String "string")]

instance FromJSON GitOid where
  parseJSON value = GitOid <$> parseJSON value

instance JsonSchema InProgressKind where
  jsonSchema _ = object
    [ ("type", String "string")
    , ("enum", Array (map (String . T.pack . show) [InProgressMerge, InProgressRebase, InProgressCherryPick, InProgressRevert, InProgressBisect]))
    ]

instance FromJSON InProgressKind where
  parseJSON = withText "InProgressKind" $ \value -> case value of
    "InProgressMerge" -> pure InProgressMerge
    "InProgressRebase" -> pure InProgressRebase
    "InProgressCherryPick" -> pure InProgressCherryPick
    "InProgressRevert" -> pure InProgressRevert
    "InProgressBisect" -> pure InProgressBisect
    _ -> Error "unknown InProgressKind"

instance FromJSON DirtySummary where
  parseJSON = withObject "DirtySummary" $ \value -> DirtySummary
    <$> value .: "staged"
    <*> value .: "unstaged"
    <*> value .: "untracked"
    <*> value .: "ignoredExcluded"

instance JsonSchema DirtySummary where
  jsonSchema _ = objectSchema Nothing
    [ ("staged", jsonSchema (Proxy @[Text]), True)
    , ("unstaged", jsonSchema (Proxy @[Text]), True)
    , ("untracked", jsonSchema (Proxy @[Text]), True)
    , ("ignoredExcluded", jsonSchema (Proxy @Int), True)
    ]

instance ToJSON HeadState where
  toJSON OnBranch { headBranch, headOid } = object
    [ "tag" .= ("OnBranch" :: Text)
    , "branch" .= headBranch
    , "oid" .= headOid
    ]
  toJSON Detached { headOid } = object
    [ "tag" .= ("Detached" :: Text)
    , "oid" .= headOid
    ]

instance FromJSON HeadState where
  parseJSON = withObject "HeadState" $ \value -> do
    tag <- value .: "tag"
    case tag :: Text of
      "OnBranch" -> do
        branch <- value .: "branch"
        oid <- value .: "oid"
        pure OnBranch { headBranch = branch, headOid = oid }
      "Detached" -> Detached <$> value .: "oid"
      _ -> Error "unknown HeadState"

instance JsonSchema HeadState where
  jsonSchema _ = object
    [ ("oneOf", Array
        [ objectSchema (Just "OnBranch")
            [ ("branch", jsonSchema (Proxy @BranchName), True)
            , ("oid", jsonSchema (Proxy @GitOid), True)
            ]
        , objectSchema (Just "Detached")
            [("oid", jsonSchema (Proxy @GitOid), True)]
        ])
    ]

instance ToJSON WorkingState where
  toJSON state = object
    (["changes" .= state.changes] <>
      case state.operation of
        Nothing -> []
        Just operation -> ["operation" .= operation])

instance FromJSON WorkingState where
  parseJSON = withObject "WorkingState" $ \value -> WorkingState
    <$> value .: "changes"
    <*> value .:? "operation"

instance JsonSchema WorkingState where
  jsonSchema _ = objectSchema Nothing
    [ ("changes", jsonSchema (Proxy @DirtySummary), True)
    , ("operation", jsonSchema (Proxy @InProgressKind), False)
    ]

instance ToJSON SubmissionObservation where
  toJSON observation = object
    [ "observedWorktreeId" .= observation.observedWorktreeId
    , "baseHead" .= observation.baseHead
    , "submittedHead" .= observation.submittedHead
    , "workingState" .= observation.workingState
    ]

instance FromJSON SubmissionObservation where
  parseJSON = withObject "SubmissionObservation" $ \value -> SubmissionObservation
    <$> value .: "observedWorktreeId"
    <*> value .: "baseHead"
    <*> value .: "submittedHead"
    <*> value .: "workingState"

instance JsonSchema SubmissionObservation where
  jsonSchema _ = objectSchema Nothing
    [ ("observedWorktreeId", jsonSchema (Proxy @WorktreeId), True)
    , ("baseHead", jsonSchema (Proxy @GitOid), True)
    , ("submittedHead", jsonSchema (Proxy @HeadState), True)
    , ("workingState", jsonSchema (Proxy @WorkingState), True)
    ]

tupleSchema :: [Value] -> Value
tupleSchema fields = object
  [ ("type", String "array")
  , ("prefixItems", Array fields)
  , ("minItems", Number (fromIntegral (length fields)))
  , ("maxItems", Number (fromIntegral (length fields)))
  ]

-- Rich construction, adaptation, and rendering helpers are ordinary library
-- code here. Thin verb wrappers and the shared 'worktreeId' projection are
-- generated from the protocol schema and re-exported above. Keeping shell/Git
-- workflow helpers out of this module avoids making every Worktree row depend
-- on Exec.

-- | Seed a managed worktree from the repository Tidepool is running
-- against. Clean-by-default: a dirty source is REFUSED unless the spec
-- is passed through 'allowDirtySnapshot'.
fromCurrentRepository :: Text -> WorktreeSpec
fromCurrentRepository lbl = WorktreeSpec SourceCurrentRepository lbl RequireClean

-- | Seed from an explicit ref (branch, tag, remote ref, or raw OID).
fromRef :: GitRef -> Text -> WorktreeSpec
fromRef r lbl = WorktreeSpec (SourceRef r) lbl RequireClean

-- | Seed from another managed worktree's current HEAD. This is how a
-- reviewer gets its own isolated tree off the branch it is reviewing.
fromWorktree :: WorktreeHandle -> Text -> WorktreeSpec
fromWorktree h lbl = WorktreeSpec (SourceWorktree (worktreeId h)) lbl RequireClean

-- | Opt IN to snapshotting a dirty source. Spelled at the call site so a
-- reader of the resident can see that a synthetic commit was taken; it
-- never alters the source branch, HEAD, index, or working-tree bytes.
allowDirtySnapshot :: WorktreeSpec -> WorktreeSpec
allowDirtySnapshot s = s { specDirtyPolicy = AllowDirtySnapshot }

-- | The managed branch this worktree is on, read fresh from git.
worktreeBranch :: Member Worktree effs => WorktreeHandle -> Eff effs BranchName
worktreeBranch h = send (WorktreeBranchOf (worktreeId h)) >>= liftEither

-- | This worktree's CURRENT @HEAD@, read fresh from git right now.
--
-- Deliberately none of the three things it could be confused with: it
-- is not the handle's recorded @sourceHead@ (the commit the managed
-- branch was rooted at), and it is not the event monitor's
-- last-observed baseline.  The whole purpose is to see what the
-- monitor did NOT.
--
-- It exists for the gap a resident spanning loop iterations has to close
-- itself.  A subscription never replays, and it lives only for its
-- loop iteration, so @HEAD@ can move after one loop iteration unregisters and before the
-- next one registers.  A resident closes that gap in ORDINARY
-- AUTHORED CODE: compare @worktreeHead tree@ against the head it
-- checkpointed, act on any difference, and only then register live
-- reactions with 'withHandler'.
--
-- That is a reinforcement of no-replay, not a loophole in it.  The
-- journal stays diagnostic rather than quietly becoming a callback
-- replay mechanism, because the resident — which knows what it already
-- acted on — decides what the gap meant, rather than the runtime
-- guessing on its behalf.
worktreeHead :: Member Worktree effs => WorktreeHandle -> Eff effs GitOid
worktreeHead h = send (WorktreeHeadOf (worktreeId h)) >>= liftEither

-- | Attach this exact managed worktree to an actor definition. The public
-- definition remains free of generic grant fields; the Actor runtime carries
-- this capability-specific recipe to deployment and binds it to the exact
-- child incarnation before an external application may use it.
withWorktree
  :: WorktreeHandle
  -> ActorDefinition startup protocol exit
  -> ActorDefinition startup protocol exit
withWorktree tree = withLaunchWorktree (renderWorktreeId (worktreeId tree))

-- | Build a 'BranchName' from a plain rendered branch name — for the case
-- (the recursive companion's fold, in particular) where a node's own domain
-- model only carries branch identity as 'Text' and needs it back as the typed
-- argument 'mergeBranchInto' takes. Infallible, same as the wire boundary's
-- own conversion: a malformed name still just fails at 'mergeBranchInto' as
-- an ordinary git failure, not a validation error here.
mkBranchName :: Text -> BranchName
mkBranchName = BranchName

renderGitOid :: GitOid -> Text
renderGitOid (GitOid t) = t

renderBranchName :: BranchName -> Text
renderBranchName (BranchName t) = t

renderWorktreeId :: WorktreeId -> Text
renderWorktreeId (WorktreeId t) = t

-- | A one-line, operator-readable rendering of a worktree failure.
-- Case-match the constructor when you mean to BRANCH on the failure;
-- this is for receipts and logs.
renderWorktreeError :: WorktreeError -> Text
renderWorktreeError (SourceDirty d) = "source repository is dirty: " <> T.pack (show (length d.staged)) <> " staged, " <> T.pack (show (length d.unstaged)) <> " unstaged, " <> T.pack (show (length d.untracked)) <> " untracked"
renderWorktreeError (NotARepository p) = "not a git repository: " <> p
renderWorktreeError (WorktreeLost i) = "managed worktree " <> renderWorktreeId i <> " is registered but missing on disk"
renderWorktreeError (DirtySubmoduleUnsupported p) = "dirty submodule is unsupported in v1: " <> p
renderWorktreeError (SourceOperationInProgress k) = "source repository has an operation in progress: " <> T.pack (show k)
renderWorktreeError (WorktreeBusy i holder) = "worktree " <> renderWorktreeId i <> " is already bound to agent " <> holder
renderWorktreeError (SubmissionUnstable i) = "worktree " <> renderWorktreeId i <> " kept changing while its submission was observed"
renderWorktreeError (WorktreeUnauthorized i) = "the executing actor is not authorized for worktree " <> renderWorktreeId i
renderWorktreeError (WorktreeAuthorityDenied detail) = "worktree authority denied: " <> detail
renderWorktreeError (GitFailure r) = "git " <> T.intercalate " " r.gitArgs <> " failed: " <> T.strip r.gitStderr
renderWorktreeError (WorktreeNotRegistered i) = "no managed worktree registered with id " <> renderWorktreeId i
renderWorktreeError (InvalidRegistryRoot root inside) = "registry root " <> root <> " resolves inside the git working tree at " <> inside <> " — the registry must live outside every source repository"
renderWorktreeError (StorageFailure p d) = "tidepool storage failure at " <> p <> ": " <> d
