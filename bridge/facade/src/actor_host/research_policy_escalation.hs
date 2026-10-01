let escalationGroup = "escalation" :: ForkGroupLabel
let escalationLabel = [label|coding|]
escalationResult <- attemptUnfoldDeferred (subgroup escalationGroup) (child (withLifetime ActorOwned (narrowed (knownEffects @ResearchEffects) (codingPolicy currentCheckout) (assignment escalationLabel ()) :: Branch ResearchEffects () Text)))
case escalationResult of { Left _ -> True; Right _ -> False }
