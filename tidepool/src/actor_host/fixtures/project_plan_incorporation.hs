let WatchReady designResult = design
let Right (AmendPlan amendment) = settledValue designResult
let Right incorporateLabel = requestLabel "incorporate-plan"
let RetainedImplementer implementer = repairOwner sessionInput
planResponse <- requestIncorporation implementer incorporateLabel (reviewAssignment sessionInput) amendment
let Right planWatch = watchLabel "plan-incorporated"
planReady <- watch planWatch (awaitSettled planResponse)
