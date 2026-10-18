import qualified Tidepool.Agent.Contract as A
import GHC.Generics (Generic)
data AmbientNominal = AmbientNominal Int
data SelectedNominal = SelectedNominal Int
data MembraneTools effects mode = MembraneTools { haskell :: mode A.:- A.HaskellCell effects } deriving Generic
let membraneSpec :: A.AgentSpec (MembraneTools '[Replies]) '[Replies]; membraneSpec = A.defaultSpec { A.specTools = MembraneTools { haskell = A.haskellTool @'A.Asynchronous @'[Replies] "Exact membrane notebook" } }
Right membraneTarget <- spawnSubagent (FreshCtx "Check exact selected nominal visibility") SameDir (defaultSpawnOptions currentModel membraneSpec)
Right membraneResponse <- request @Int membraneTarget (SelectedNominal 42) defaultRequestOptions
