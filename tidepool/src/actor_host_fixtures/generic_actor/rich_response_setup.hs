data Probe = Probe { question :: String, contract :: String } deriving Show
data Report = Report { findings :: [String], validation :: [String], candidate :: Maybe String } deriving Show
type Review = Report
let scaffold = Probe "Does speed zero freeze live B against pinned A, and does overlap prove identical samples?" "sample motion = millis*speed/400; noise hashes time independently; plot quantizes samples to rows."
let domainPlan = (scaffold, "Coding assignment: validate speed-zero/noise counterexample with focused Rust tests in your isolated worktree. Return typed evidence with exact commands and values.")
let reviewPlan = (scaffold, "Read-only assignment: inspect pinned reference and projection behavior, derive concrete overlap/resize counterexamples. Keep computations resident Haskell; return typed evidence.")
let consumerPlan = (scaffold, "Consumer assignment: independently test the shared contract, including failure behavior. Return exact checks, discoveries, and limits.")
