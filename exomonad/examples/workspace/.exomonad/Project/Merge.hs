{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- Publishing a checked revision. `Merge` is the worktree-holding record
-- actor: one per merge target, called by every review under a parent. It
-- merges one candidate per `Call`, runs the project check on the merged head, and
-- only then advances the named branch -- a red head is rolled back so the
-- worktree always sits on a green revision between calls. Read by whoever
-- starts a merge target and by Project.Review, which is the only caller of
-- `publish`.
--
-- Publishing a checked revision is a separate actor from the review. Worktree
-- custody is exclusive and publish authority follows custody, so the actor
-- that merges is the actor that holds the integration worktree: one `Merge`
-- per merge target, started with `R.withWorktree` on a worktree the parent
-- created and never bound. Reviews hold no worktree (they resolve to the
-- research role) and `R.call` the merge actor; its mailbox serialises every
-- merge-and-check. The merge actor checks before it publishes: the project check
-- runs on the merged head, a green head advances the named branch, a red head
-- is rolled back so the worktree is always on a green revision between calls.
-- The same two actors run from any node: a Sol or Luna node creates its own
-- integration worktree from its bound head (the coding role may allocate),
-- starts its own merge actor, and replies to its parent with the same
-- `ImplReport` a leaf sends -- integrated head, aggregate changed paths,
-- literal check output, unresolved conditions -- so the parent's review
-- checks the whole subtree diff against the node's contract exactly as it
-- checks a leaf.
module Project.Merge
  ( -- The merge target: supplied by whoever starts a review
    MergeTarget (..)
    -- The merge actor: one per merge target
  , Merge (..)
  , MergeEffects
  , MergeState (..)
  , PublishRequest (..)
  , MergeResult (..)
  , mergeInto
    -- Shared with Project.Review, which runs its own `git` commands
  , exitCode
  ) where

import Data.Maybe (isNothing)
import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)

import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import qualified Tidepool.Command as Cmd
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core (BranchName (..), Commands, WorktreeHandle (..), WorktreeReceipt (..))

import Project.Evidence (CheckResult (..), CheckSource (..), HistoryEntry (..))
import Project.Reflex (reflexFor)

-- The merge target, supplied by whoever starts the review: the merge actor
-- that holds the integration worktree. Every review under one parent shares
-- it.
newtype MergeTarget = MergeTarget
  { mergeActor :: R.ActorHandle Merge
  } deriving (Show)

data PublishRequest = PublishRequest
  { publishTask :: Text
  , publishCandidate :: GitOid
  , publishMessage :: Text
  } deriving (Show, Eq)

-- Every outcome names the revisions it checked or refused: a merge that
-- landed and then failed its check is a different fact from a merge that
-- failed, and the parent reads which from the constructor.
data MergeResult
  = Published GitOid GitOid CheckResult
    -- ^ checked head, previous head, the green check; the branch advanced
  | RedRolledBack GitOid GitOid CheckResult
    -- ^ checked head, the head the worktree was rolled back to, the red check
  | Conflict Text [Text]
  | MergeBlocked Text
    -- ^ the merge actor refuses every request until `reconcile` clears the
    -- reason: publication drift, a rollback that did not restore the head,
    -- an advance that was refused, or the publication branch checked out
    -- somewhere `update-ref` would desynchronise
  | MergeFailed Text
  deriving (Show, Eq)

-- ---------------------------------------------------------------------------
-- The merge actor: one per merge target. It holds the integration worktree
-- (custody is exclusive, publish authority follows custody), merges one
-- candidate per Call, checks the merged head, and only then publishes.
-- ---------------------------------------------------------------------------

data Merge mode = Merge
  { mergeState :: mode :- State MergeState
  , publish :: mode :- Call PublishRequest (R.Reply MergeResult)
  , reconcile :: mode :- Call Text NoReply
    -- ^ the parent, after fixing the worktree and branch by hand, clears the
    -- blocked state with a note that lands in the history
  , mergeView :: mode :- Call () (R.Reply MergeState)
  } deriving Generic

data MergeState = MergeState
  { mergeAdvanceBranch :: Maybe BranchName
  , mergeCheckCommand :: [Text]
    -- ^ what this actor runs to decide a merged head is green. Supplied at
    -- start, never guessed, and shown in the state so a receipt always says
    -- which command produced its verdict.
  , mergeTree :: Maybe WorktreeHandle
  , mergeBlocked :: Maybe Text
  , mergeHistory :: [HistoryEntry]
  }

instance Show MergeState where
  show state = unlines $
    ( "merge tree=" ++ maybe "unbound" (Text.unpack . cwd . handleReceipt) (mergeTree state)
      ++ " advance=" ++ maybe "-" (\(BranchName branch) -> Text.unpack branch) (mergeAdvanceBranch state)
      ++ " check=" ++ Text.unpack (Text.unwords (mergeCheckCommand state))
      ++ " blocked=" ++ maybe "no" Text.unpack (mergeBlocked state)
    ) : map show (mergeHistory state)

type MergeEffects = R.LocalEffects Merge
  '[Replies, BoundWorktree, WorktreeIntegration, Commands, Actor]

-- | Start with the id of a worktree the parent created and did not bind, and
-- the command that decides a merged head is green:
--
-- > Right tree <- createWorktree (fromRef "exomonad/integration" "integration")
-- > merge <- R.start (mergeInto (worktreeId tree) (Just "exomonad/integration")
-- >                     ["just", "test-lib", "exomonad-actor", "test(request::updates)"])
--
-- The check is an argument because only the caller knows what green means for
-- the change in hand. Name the narrowest command that would actually catch a
-- regression in it — a crate and a test filter, not a whole workspace.
--
-- This actor came from a project whose entire check was a one-second script, so
-- running it after every merge cost nothing. That is not true here: a
-- workspace-wide run in a fresh worktree compiles the world first. Do not pass
-- `just verify`; it is the pre-review gate, budgeted at up to two hours, and an
-- actor must not start it unattended.
mergeInto :: WorktreeId -> Maybe BranchName -> [Text] -> ActorSpec Merge MergeEffects
mergeInto tree advance check =
  R.withWorktree tree $ R.definition "integrator" (Actor.Selected knownEffects) Merge
    { mergeState = MergeState advance check Nothing Nothing []
    , mergeView = \() -> R.get
    , publish = runPublish
    , reconcile = \note -> R.modify' (\state -> state
        { mergeBlocked = Nothing
        , mergeHistory = mergeHistory state
            ++ [ HistoryEntry (length (mergeHistory state)) "integrate" Nothing RanHere
                   "reconciled" note "accepting requests again" ] })
    }

runPublish :: PublishRequest -> Handler MergeState MergeEffects MergeResult
runPublish request = do
  blocked <- R.gets mergeBlocked
  bound <- ownTree
  case (blocked, bound) of
    (Just reason, _) -> do
      history "refused_while_blocked" reason
      pure (MergeBlocked reason)
    (Nothing, Left failure) -> do
      history "unbound" (Text.pack (show failure))
      pure (MergeFailed ("the merge actor holds no worktree: " <> Text.pack (show failure)))
    (Nothing, Right handle) -> do
      let path = cwd (handleReceipt handle)
      advance <- R.gets mergeAdvanceBranch
      before <- gitIn path ["rev-parse", "HEAD"]
      -- Publication drift is established before the merge, not discovered
      -- by a failed final command: the branch must be where the worktree is,
      -- and it must not be checked out anywhere `update-ref` would leave a
      -- stale index behind.
      drift <- case advance of
        Nothing -> pure Nothing
        Just (BranchName branch) -> do
          published <- gitIn path ["rev-parse", "refs/heads/" <> branch]
          checkouts <- gitIn path ["worktree", "list", "--porcelain"]
          let elsewhere = [ line | line <- Text.lines checkouts, line == "branch refs/heads/" <> branch ]
          pure $ if published /= before
            then Just ("publication branch " <> branch <> " is at " <> Text.take 7 published
                       <> " but the integration worktree is at " <> Text.take 7 before)
            else if not (null elsewhere)
            then Just ("publication branch " <> branch <> " is checked out in another worktree; update-ref would desynchronise it")
            else Nothing
      case drift of
        Just reason -> block "drift" reason
        Nothing -> do
          outcome <- tryMerge MergeRequest
            { mergeSourceHead = publishCandidate request
            , mergeSourceBranch = Nothing
            , mergeTargetWorktree = worktreeId handle
            , mergeMessage = publishMessage request
            , mergeAdvance = Nothing
            }
          case outcome of
            Left failure -> do
              history "merge_error" (Text.pack (show failure))
              pure (MergeFailed (Text.pack (show failure)))
            Right (ManualGitRequired _ _ reason paths) -> do
              history "conflict" (reason <> ": " <> Text.intercalate ", " paths)
              pure (Conflict reason paths)
            Right merged -> do
              checked <- gitIn path ["rev-parse", "HEAD"]
              check <- checkedOutput path
              if checkPassed check
                then do
                  published <- case advance of
                    Nothing -> pure Nothing
                    Just (BranchName branch) -> do
                      result <- Cmd.run (Cmd.inDirectory path
                        (Cmd.argv ["git", "update-ref", "refs/heads/" <> branch, checked, before]))
                      -- `update-ref` says why it refused on stderr; reading
                      -- stdout here reported an empty reason for a real
                      -- failure, and the caller inferred a cause that was
                      -- false (dogfood: the ref file was unwritable, not moved).
                      pure (Cmd.failure result)
                  case published of
                    Nothing -> do
                      history "merged_green" (Text.pack (show merged) <> "; " <> checked <> "; " <> checkDetail check)
                      pure (Published (GitOid checked) (GitOid before) check)
                    Just detail ->
                      block "advance_refused"
                        ("the checked head was not published to " <> branchText advance
                          <> ": " <> detail)
                else do
                  -- The rollback takes the red merge commit off the worktree's
                  -- ref, so it names the tip it checked: the discard hold
                  -- refuses it if anything else moved the worktree meanwhile.
                  let intent = DiscardIntent (GitOid checked) (GitOid before) "red check rollback"
                  reset <- Cmd.run (withDiscardIntent intent
                    (Cmd.inDirectory path (Cmd.argv ["git", "reset", "--hard", before])))
                  restored <- gitIn path ["rev-parse", "HEAD"]
                  if isNothing (Cmd.failure reset) && restored == before
                    then do
                      history "merged_red_rolled_back" (checked <> " -> " <> before <> "; " <> checkDetail check)
                      pure (RedRolledBack (GitOid checked) (GitOid before) check)
                    else block "rollback_failed"
                      ("the worktree is at " <> Text.take 7 restored <> ", expected " <> Text.take 7 before
                        <> " after a red check at " <> Text.take 7 checked
                        <> maybe "" ("; git reset " <>) (Cmd.failure reset))
  where
    history :: Text -> Text -> Handler MergeState MergeEffects ()
    history key detail = R.modify' (\state -> state
      { mergeHistory = mergeHistory state
          ++ [ HistoryEntry (length (mergeHistory state)) "integrate"
                 (Just (publishCandidate request)) RanHere key
                 (publishTask request <> ": " <> detail) "replied to the review" ] })
    block :: Text -> Text -> Handler MergeState MergeEffects MergeResult
    block key reason = do
      R.modify' (\state -> state { mergeBlocked = Just reason })
      history key reason
      pure (MergeBlocked reason)

ownTree :: Handler MergeState MergeEffects (Either WorktreeError WorktreeHandle)
ownTree = do
  cached <- R.gets mergeTree
  case cached of
    Just handle -> pure (Right handle)
    Nothing -> do
      bound <- boundWorktree
      case bound of
        Right handle -> do
          R.modify' (\state -> state { mergeTree = Just handle })
          pure bound
        Left _ -> pure bound

-- The publication branch as the reader named it, for a message about it.
branchText :: Maybe BranchName -> Text
branchText = maybe "no publication branch" (\(BranchName name) -> name)

gitIn :: Text -> [Text] -> Handler MergeState MergeEffects Text
gitIn path arguments = do
  result <- Cmd.run (Cmd.inDirectory path (Cmd.argv ("git" : arguments)))
  pure (Text.strip (either (const "") id (Cmd.stdout result)))

exitCode :: Cmd.RunResult -> Int
exitCode result = case Cmd.commandOutcome (Cmd.commandResult result) of
  Cmd.CommandExited status -> status
  _ -> 1

outputOf :: Cmd.RunResult -> Text
outputOf result = Cmd.outputText (Cmd.commandStdout (Cmd.capturedOutput result))

checkedOutput :: Text -> Handler MergeState MergeEffects CheckResult
checkedOutput path = do
  check <- R.gets mergeCheckCommand
  result <- Cmd.run (Cmd.inDirectory path (Cmd.argv check))
  -- A check that fails usually says why on stderr; classify over both streams
  -- so the reflex table sees the compiler's own words.
  let spoken = Text.strip (outputOf result <> "\n" <> Cmd.stderr result)
      code = exitCode result
  pure (CheckResult (Text.unwords check) RanHere (code == 0)
    (maybe (Text.takeEnd 400 spoken) (Text.pack . show) (reflexFor code spoken)))
