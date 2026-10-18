import Tidepool.Actors.Exomonad
import qualified Tidepool.Effects.Core as Core
import qualified StartupSpec as Startup
let startupOptions spec label = (defaultSpawnOptions currentModel spec) { spawnLifetime = ActorOwned, spawnLabel = Just label }
Right startupFresh <- spawnSubagent (FreshCtx "fresh-caller-seed") SameDir (startupOptions (Startup.agentSpec @'[Core.FsRead, Core.FsWrite, Core.Jev, Replies]) "startup-fresh")
Right startupEmptyCaller <- spawnSubagent (FreshCtx "") SameDir (startupOptions (Startup.agentSpec @'[Core.FsRead, Core.FsWrite, Core.Jev, Replies]) "startup-empty-caller")
Right startupCapture <- checkpoint currentModel "startup child captured context"
Right startupCaptured <- spawnSubagent (ForkCtx startupCapture) SameDir (startupOptions (Startup.agentSpec @'[Core.FsRead, Core.FsWrite, Core.Jev, Replies]) "startup-captured")
Right startupEmpty <- spawnSubagent (FreshCtx "empty-result-caller") SameDir (startupOptions (Startup.emptySpec @'[Replies]) "startup-empty-result")
Right startupAbsent <- spawnSubagent (FreshCtx "absent-hook-caller") SameDir (startupOptions (Startup.absentSpec @'[Replies]) "startup-absent")
display True
