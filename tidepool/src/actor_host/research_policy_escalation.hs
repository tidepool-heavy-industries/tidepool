let escalationGroup = "escalation" :: ForkGroupLabel
let escalationLabel = "coding" :: Label
escalationResult <- attemptUnfold (subgroup escalationGroup) (child (narrowed (knownEffects @ResearchEffects) (codingPolicy boundHead) escalationLabel () :: Branch ResearchEffects () Text))
case escalationResult of { Left _ -> True; Right _ -> False }
