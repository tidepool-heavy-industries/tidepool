let escalationGroup = "escalation" :: ForkGroupLabel
let escalationLabel = [label|coding|]
escalationResult <- attemptUnfold (subgroup escalationGroup) (child (narrowed (knownEffects @ResearchEffects) (codingPolicy boundHead) (assignment escalationLabel ()) :: Branch ResearchEffects () Text))
case escalationResult of { Left _ -> True; Right _ -> False }
