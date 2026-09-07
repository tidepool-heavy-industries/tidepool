reviewState <- pollWatch againReady
let WatchReady (ReplyAvailable reviewAnswer) = reviewState
let Produced (Accepted accepted) = responseValue reviewAnswer
respond (Produced (Delivered accepted (candidateCommit (reviewedCandidate accepted)) ["integration content check"]))
