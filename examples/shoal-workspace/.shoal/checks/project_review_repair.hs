let Right repairLabel = requestLabel "repair-candidate"
next <- repair repairLabel sessionInput (reviewInput sessionInput) ["preserve the product gate"]
let Right revision = next
let Right repairedLabel = watchLabel "repaired"
repaired <- watch repairedLabel (awaitSettled revision)
