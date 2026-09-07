let Right repairLabel = requestLabel "repair-candidate"
revision <- requestRepair repairLabel sessionInput (reviewInput sessionInput) ["preserve the product gate"]
let Right repairedLabel = watchLabel "repaired"
repaired <- watch repairedLabel (awaitSettled revision)
:{
repairValue :: Settlement Candidate -> Either ResponseFailure Candidate
repairValue (ReplyAvailable answer) = Right (responseValue answer)
repairValue (ReplyUnavailable failure) = Left failure
:}
