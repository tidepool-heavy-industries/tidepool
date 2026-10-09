import Tidepool.Actors.Exomonad
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Worktree as Wt
import qualified Tidepool.Agent.Contract as A
Right selectedTree <- send (Core.WorktreeAllocationCreate (Wt.fromCurrentRepository "deferred-reply-directory"))
Right selectedWorkspace <- Wt.workspaceFor selectedTree
let deferredOptions = (defaultSpawnOptions (A.defaultAsyncWorkbenchSpec @'[Replies, Core.Jev, Core.Console])) { spawnLifetime = ActorOwned, spawnLabel = Just "deferred-reply-child" }
Right deferredChild <- spawnSubagent (FreshCtx "Receive and reply to the typed assignment.") (ExistingWorkspace selectedWorkspace) deferredOptions
Right deferredJob <- request @Text deferredChild ("work" :: Text) (defaultRequestOptions { requestLabel = Just "deferred-reply-request" })
deferredWatch <- watch (Just "deferred-reply-watch") (result deferredJob)
display True
