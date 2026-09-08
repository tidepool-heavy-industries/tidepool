current <- snapshot
let base = (head (snapshotActors current)) { rosterProviderObservationStale = False, rosterCurrentRequests = [], rosterQueuedRequests = [], rosterProviderHealth = ProviderSucceeded, rosterDisposition = Just IdleRetained }
let rows =
      [ base { rosterActorId = 1, rosterState = RosterRunning, rosterDisposition = Just Working }
      , base { rosterActorId = 2, rosterState = RosterFailed "failed" }
      , base { rosterActorId = 3, rosterState = RosterCancelled "custody unknown" }
      , base { rosterActorId = 4, rosterState = RosterStopped }
      , base { rosterActorId = 5, rosterState = RosterStopped, rosterProviderHealth = ProviderActive }
      , base { rosterActorId = 6, rosterState = RosterStopped, rosterProviderObservationStale = True }
      , base { rosterActorId = 7, rosterState = RosterRunning }
      , base { rosterActorId = 8, rosterState = RosterStopped, rosterProviderHealth = ProviderUnknown }
      , base { rosterActorId = 9, rosterState = RosterRunning, rosterCurrentRequests = [42] }
      ]
let focusedActors = workingAndAbnormal (SwarmSnapshot rows)
inspectFull (map rosterActorId (snapshotActors focusedActors) == [1,2,3,5,6,8,9] && length (actorSummary focusedActors) == 7)
