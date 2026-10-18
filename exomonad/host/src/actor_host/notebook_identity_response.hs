import qualified Tidepool.Agent.Contract as A
data CellReply = CellReply Text deriving Show
Right workerCapture <- checkpoint currentModel "typed worker fixture"
Right reviewer <- spawnSubagent (ForkCtx workerCapture) SameDir ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "nominal-reviewer" })
Right pending <- request @CellReply reviewer () defaultRequestOptions
let pinned = pending :: Request CellReply
pollResponse pinned
let later = pollResponse pinned
later >>= display
