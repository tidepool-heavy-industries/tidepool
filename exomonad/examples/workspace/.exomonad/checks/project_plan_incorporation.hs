{-# LANGUAGE QuasiQuotes #-}
let AssignedTask assignedTask = reviewBasis sessionInput
let WatchReady designResult = design
let Right (AmendPlan amendment) = settledValue designResult
let incorporateLabel = [label|incorporate-plan|]
let RetainedImplementer implementer = repairOwner sessionInput
planResponse <- requestIncorporation implementer incorporateLabel assignedTask amendment
let planWatch = "plan-incorporated" :: WatchLabel
planReady <- watch planWatch (awaitSettled planResponse)
