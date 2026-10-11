import qualified Tidepool.Agent.Contract as A
import GHC.Generics (Generic)
data AmbientNominal = AmbientNominal Int
data MembraneTools effects mode = MembraneTools { haskell :: mode :- A.HaskellCell effects } deriving Generic
let membraneSpec :: A.AgentSpec (MembraneTools '[Replies]) '[Replies]; membraneSpec = A.defaultSpec { A.specTools = MembraneTools { haskell = A.haskellTool @'A.Asynchronous @'[Replies] "Exact membrane notebook" } }
Right membraneTarget <- spawnSubagent (FreshCtx "Check exact selected nominal visibility") SameDir (defaultSpawnOptions membraneSpec)
Right membraneResponse <- request @Int membraneTarget () defaultRequestOptions
