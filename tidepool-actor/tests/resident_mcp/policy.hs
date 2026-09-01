let childDefinition :: ActorDefinition Int Maybe Int
    childDefinition =
      ActorDefinition
        { label = "spawned-by-tool"
        , effectProfile = ReadOnly
        , initialization = \seed -> pure seed
        , behavior = \_ initial ->
            (pure initial :: Eff (ReadOnlyEffects Maybe) Int)
        , visibleToChild = []
        , onShutdown = const (pure ())
        }

    tools :: ResidentTools (AsServerT (Eff ActorEffects))
    tools =
      ResidentTools
        { doubleValue =
            tool "Double one integer." $ \request ->
              pure (EchoOutput (request.value * 2))
        , spawnChild =
            tool "Start one supervised child actor." $ \request -> do
              _ <- startActor childDefinition request.seed
              pure (SpawnOutput True)
        }
in serveTools tools
