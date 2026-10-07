import Tidepool.Actors.Exomonad
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Worktree as Wt
import qualified Project.Supplied as Supplied
Right attachmentTree <- send (Core.WorktreeAllocationCreate (Wt.fromCurrentRepository "child-attachment-directory"))
Right attachmentWorkspace <- Wt.workspaceFor attachmentTree
let attachmentOptions = defaultSpawnOptions (Supplied.actualSpec 101 "The caller supplied this distinctive probe.")
attachmentFailure <- spawnSubagent (FreshCtx "This child must fail attachment without activating inference.") (ExistingWorkspace attachmentWorkspace) (attachmentOptions { spawnLabel = Just "unknown-alias-child", spawnModel = Just (Alias "deliberately-unknown-child-alias") })
display (case attachmentFailure of { Left (SpawnPartialFailure (SpawnRetainedActor child) _ _) -> case agentBoundWorktree child of { Just tree -> Wt.worktreeId tree == Wt.worktreeId attachmentTree; _ -> False }; _ -> False })
