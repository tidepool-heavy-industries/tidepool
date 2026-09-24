{-# LANGUAGE QuasiQuotes #-}
let repairLabel = [label|repair-candidate|]
next <- repair repairLabel sessionInput (reviewInput sessionInput) ["preserve the product gate"]
let Right revision = next
let repairedLabel = "repaired" :: WatchLabel
repaired <- watch repairedLabel (awaitSettled revision)
