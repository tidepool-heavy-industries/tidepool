let WatchReady designResult = design
let Right (AmendPlan amendment) = settledValue designResult
let incorporateLabel = "incorporate-plan" :: Label
let RetainedImplementer implementer = repairOwner sessionInput
planResponse <- requestIncorporation implementer incorporateLabel (reviewAssignment sessionInput) amendment
let planWatch = "plan-incorporated" :: WatchLabel
planReady <- watch planWatch (awaitSettled planResponse)
