import GHC.Generics (Generic)
data OptionalObserver mode = OptionalObserver { observerPrivate :: mode :- State (), observeOptional :: mode :- Call (WorkEvent Text) NoReply } deriving Generic
let optionalDefinition = coordinationActor "optional-observer" OptionalObserver { observerPrivate = (), observeOptional = \_ -> error "observer failure" }
optional <- R.start optionalDefinition
let staleObservation = observeOptional (R.client optional)
optional <- R.replace optional optionalDefinition
let optionalSink = observeWork "stale" staleObservation Just (observeWork "failed-handler" (observeOptional (R.client optional)) Just (notifyWork me (workMessage id)))
Right collection <- followWork [("producer", producer, updates)] optionalSink
