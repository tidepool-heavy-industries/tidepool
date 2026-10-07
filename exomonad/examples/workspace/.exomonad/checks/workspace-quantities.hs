import qualified Project.Work as Work
import qualified Tidepool.Agent.Contract as Contract
import Tidepool.Scope (ScopeOutcome (..), withScope)
import Tidepool.Duration (seconds)
import Tidepool.Effects (sleep)
let scopedWorkspaceSleep :: Eff Work.WorkspaceEffects (ScopeOutcome ())
    scopedWorkspaceSleep = withScope (\_ -> sleep (seconds 3))
workspaceSleepOutcome <- scopedWorkspaceSleep
let workspaceProfileKeys = case Contract.compileInstalledTools (Contract.specTools Work.workspaceAgentSpec) of
      Left failure -> error (show failure)
      Right installed -> [keys | entry <- Contract.declarations installed, Contract.dtdImplementation entry == Contract.NativeHaskellCell, Just keys <- [Contract.dtdEffectKeys entry]]
