import Tidepool.Actors.Observe (actorContext)

context <- actorContext
let self = (contextActorId context, contextActorIncarnation context)
SwarmSnapshot descendants <- creationTree self <$> snapshot
let live = [ actor | actor <- descendants
                    , (rosterActorId actor, rosterActorIncarnation actor) /= self
                    , rosterState actor == RosterRunning
                    ]
inspectFull live
