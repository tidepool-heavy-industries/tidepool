{-# LANGUAGE QuasiQuotes #-}
let AssignedTask assignedTask = reviewBasis sessionInput
let WatchReady designResult = design
let Right (AmendPlan amendment) = settledValue designResult
let incorporateLabel = [label|incorporate-plan|]
let RetainedImplementer implementer = repairOwner sessionInput
planHandoff <- requestIncorporation implementer incorporateLabel assignedTask amendment
let planResponse = handedRequest planHandoff
let planWatch = "plan-incorporated" :: WatchLabel
planReady <- case handoffRetention planHandoff of
  Right () -> watch planWatch (awaitSettled planResponse)
  Left issue -> error (show issue)
