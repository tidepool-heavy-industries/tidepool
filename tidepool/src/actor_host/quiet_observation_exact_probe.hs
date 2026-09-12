case __NAME__ of
  WatchReady reply ->
    let value = responseValue reply
    in candidateRevision value == "candidate-9828"
       && testedRevision value == "tested-6c6c"
       && length (checkEvidence value) == 2000
       && limitations value == ["LIMITATION-MUST-REMAIN-AVAILABLE"]
  _ -> False
