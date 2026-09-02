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

    tools current =
      ResidentTools
        { doubleValue =
            tool "Double one integer." $ \request ->
              pure (EchoOutput (request.value * 2))
        , spawnChild =
            tool "Start one supervised child actor." $ \request -> do
              ref <- startActor childDefinition request.seed
              first <- awaitExit ref
              second <- awaitExit ref
              pure
                ( SpawnOutput
                    ( first == Completed request.seed
                        && second == Completed request.seed
                    )
                )
        , currentValue =
            tool "Return the current resident state." $ \_ ->
              pure (StateOutput current)
        , setValue =
            updateTool "Replace the resident state." $ \request ->
              pure (StateOutput request.next, request.next)
        , finishValue =
            finishTool "Reply and complete with the current state." $ \_ ->
              pure (StateOutput current, current)
        }
in serveToolsWith 0 tools
