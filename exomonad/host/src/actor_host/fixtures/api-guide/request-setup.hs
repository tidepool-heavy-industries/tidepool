import qualified Tidepool.Agent.Contract as A
let prompt = "Return the supplied typed input."
    input = "Inspect this candidate." :: Text
    spec = A.defaultWorkbenchSpec @'[Replies]
