import Tidepool.Actors.Exomonad
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Worktree as Wt
import qualified Project.Supplied as Supplied
Right selectedTree <- send (Core.WorktreeAllocationCreate (Wt.fromCurrentRepository "supplied-live-directory"))
Right selectedWorkspace <- Wt.workspaceFor selectedTree
let capturedOffset = 101 :: Int
let suppliedOptions = (defaultSpawnOptions (Supplied.actualSpec capturedOffset "The caller supplied this distinctive probe.")) { spawnLifetime = ActorOwned }
Right suppliedChild <- spawnSubagent (FreshCtx "Fresh supplied-spec context, awaiting a separate request.") (ExistingWorkspace selectedWorkspace) (suppliedOptions { spawnLabel = Just "supplied-live-first" })
Right suppliedSibling <- spawnSubagent (FreshCtx "Another idle child using the same supplied value.") (ExistingWorkspace selectedWorkspace) (suppliedOptions { spawnLabel = Just "supplied-live-second" })
display (case (agentBoundWorktree suppliedChild, agentBoundWorktree suppliedSibling) of { (Just first, Just second) -> Wt.worktreeId first == Wt.worktreeId selectedTree && Wt.worktreeId second == Wt.worktreeId selectedTree; _ -> False })
