let repairLabel = "repair-candidate" :: Label
next <- repair repairLabel sessionInput (reviewInput sessionInput) ["preserve the product gate"]
let Right revision = next
let repairedLabel = "repaired" :: WatchLabel
repaired <- watch repairedLabel (awaitSettled revision)
