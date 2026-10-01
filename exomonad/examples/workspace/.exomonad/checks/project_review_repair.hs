{-# LANGUAGE QuasiQuotes #-}
let repairLabel = [label|repair-candidate|]
next <- repair repairLabel sessionInput (reviewInput sessionInput) ["preserve the product gate"]
let Right handoff = next
let revision = handedRequest handoff
let repairedLabel = "repaired" :: WatchLabel
repaired <- case handoffRetention handoff of
  Right () -> watch repairedLabel (awaitSettled revision)
  Left issue -> error (T.pack (show issue))
