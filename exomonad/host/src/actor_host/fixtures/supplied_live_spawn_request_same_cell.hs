import Tidepool.Actors.Exomonad
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Worktree as Wt
import qualified Project.Supplied as Supplied
Right selectedTree <- send (Core.WorktreeAllocationCreate (Wt.fromCurrentRepository "supplied-live-immediate-directory"))
Right selectedWorkspace <- Wt.workspaceFor selectedTree
let immediateOptions = (defaultSpawnOptions (Supplied.actualSpec 101 "The caller supplied this distinctive probe.")) { spawnLifetime = ActorOwned }
Right immediateChild <- spawnSubagent (FreshCtx "Fresh supplied-spec context, immediately receiving a typed request.") (ExistingWorkspace selectedWorkspace) (immediateOptions { spawnLabel = Just "supplied-live-immediate" })
Right immediateJob <- request @Int immediateChild (30 :: Int) (defaultRequestOptions { requestLabel = Just "supplied-live-immediate-request" })
display True
