:{
data LateRefinement = LateRefinement
  { refinementValue :: Int
  , refinementTransform :: Int -> Int
  }
:}
:{
data LateReport = LateReport Int deriving Show
:}
let capturedTransform = \value -> value + sharedDelta
let sharedDelta = 10 :: Int
type RevisionReport = LateReport
let interfaceWorker = first3 workers
let Right revisionLabel = requestLabel "revision"
let revisionPlan = LateRefinement 99 capturedTransform
