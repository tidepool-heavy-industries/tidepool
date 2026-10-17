import Tidepool.Actors.Exomonad
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Worktree as Wt
import qualified Project.Supplied as Supplied
Right attachmentTree <- send (Core.WorktreeAllocationCreate (Wt.fromCurrentRepository "child-attachment-directory"))
Right attachmentWorkspace <- Wt.workspaceFor attachmentTree
let attachmentOptions = defaultSpawnOptions (Supplied.actualSpec 101 "The caller supplied this distinctive probe.")
attachmentFailure <- spawnSubagent (FreshCtx "This child must fail attachment without activating inference.") (ExistingWorkspace attachmentWorkspace) (attachmentOptions { spawnLabel = Just "wrong-backend-child", spawnModel = Just DeepSeekFlash })
display (case attachmentFailure of { Left (SpawnRefused _) -> True; _ -> False })
