let childDefinition :: ActorDefinition Int Maybe Int
    childDefinition =
      ActorDefinition
        { label = "failed-host-cleanup-child"
        , effectProfile = ReadOnly
        , initialization = \seed -> pure seed
        , behavior = \_ initial -> (pure initial :: Eff (ReadOnlyEffects Maybe) Int)
        , onShutdown = const (error "authored shutdown hook failure")
        }
    tools current =
      ResidentTools
        { spawnChild =
            tool "Start one supervised child actor." $ \request -> do
              ref <- startActor childDefinition request.seed
              observed <- awaitExit ref
              pure (SpawnOutput (observed == Completed request.seed))
        }
in serveToolsWith () tools
