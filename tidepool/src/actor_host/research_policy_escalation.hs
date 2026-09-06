let escalationGroup = case forkGroupLabel "escalation" of { Right value -> value; Left _ -> error "fixture label" }
let escalationLabel = case branchLabel "coding" of { Right value -> value; Left _ -> error "fixture label" }
escalationResult <- attemptUnfold (subgroup escalationGroup) (child (narrowed (knownEffects @ResearchEffects) (codingPolicy boundHead) escalationLabel () :: Branch ResearchEffects () Text))
case escalationResult of { Left _ -> True; Right _ -> False }
