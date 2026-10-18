firstRelease <- releaseCheckpoint seed
secondRelease <- releaseCheckpoint seed
outcomes <- mapM stopAgent [alpha, beta]
let stopped = all (\outcome -> case outcome of { StoppedNow -> True; AlreadyStopped -> True; _ -> False }) outcomes
case (firstRelease, secondRelease) of
  (Right (), Right ()) | stopped -> display True >> pure ()
  _ -> display (show (firstRelease, secondRelease, outcomes)) >> pure ()
