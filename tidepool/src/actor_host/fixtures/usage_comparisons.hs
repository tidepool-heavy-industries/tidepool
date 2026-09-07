observed <- snapshot
let base = head (snapshotActors observed)
let row actor thread model count total = base { rosterActorId = actor, rosterRequestedModel = model, rosterFirstUsage = Just (ProviderUsageObservation "first" Nothing 0 0), rosterUsageSummary = Just (ProviderUsageSummary (UsageThread thread) UsageComplete count total 0 0 0 total) }
let old = row 101 "thread" (Just "sol") 1 10
let newer = row 102 "thread" (Just "sol") 3 30
let other = row 103 "other" (Just "astra") 1 7
let total rows = totalTokens (swarmUsage (SwarmSnapshot rows))
let before = SwarmSnapshot [old]
let after = SwarmSnapshot [old, newer, other]
let changed = newer { rosterFirstUsage = Just (ProviderUsageObservation "different-source" Nothing 0 0) }
let mixed = newer { rosterRequestedModel = Just "astra" }
let comparisons = usageDelta before after
let partialRow = newer { rosterUsageSummary = fmap (\usage -> usage { usageSummaryCompleteness = UsagePartial }) (rosterUsageSummary newer) }
let partialChange = usageDelta before (SwarmSnapshot [partialRow])
let unknownRow = base { rosterActorId = 104, rosterUsageSummary = Nothing }
let checks = [total [old,newer] == 30, total [newer,old] == 30, total [old,newer,other] == 37, totalTokens (comparableUsage comparisons) == 20, totalTokens (newlyObservedUsage comparisons) == 7, lostProviderThreads (usageDelta after before) == ["other"], discontinuousProviderThreads (usageDelta after before) == ["thread"], totalTokens (comparableUsage (usageDelta before (SwarmSnapshot [changed]))) == 0, discontinuousProviderThreads (usageDelta before (SwarmSnapshot [changed])) == ["thread"], inconsistentProviderThreads (swarmUsage (SwarmSnapshot [old,changed])) == ["thread"], map (\(model,usage) -> (model,totalTokens usage)) (usageByRequestedModel after) == [(Just "sol",30),(Just "astra",7)], map (\(model,usage) -> (model,totalTokens usage)) (usageByRequestedModel (SwarmSnapshot [old,mixed])) == [(Nothing,30)]]
inspectFull (and checks && partialProviderThreads (comparableUsage partialChange) == ["thread"] && totalTokens (comparableUsage partialChange) == 20 && length (unknownActors (swarmUsage (SwarmSnapshot [unknownRow]))) == 1 && totalTokens (newlyObservedUsage (usageDelta (SwarmSnapshot [unknownRow]) (SwarmSnapshot [newer]))) == 30)
