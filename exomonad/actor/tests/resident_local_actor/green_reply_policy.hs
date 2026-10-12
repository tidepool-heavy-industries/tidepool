let childDefinition :: ActorDefinition Int Maybe Int
    childDefinition =
      ActorDefinition
        { label = "spawned-by-tool"
        , effectProfile = Selected (knownEffects @'[])
        , initialization = \seed -> pure seed
        , behavior = \_ initial ->
            (pure initial :: Eff '[ActorLocal Maybe] Int)
        , onShutdown = const (pure ())
        }

    tools current =
      ResidentTools
        { doubleValue =
            tool "Double one integer." $ \request -> do
              _ <- Async.async (pure ())
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
            finishTool "Reply and complete with the current state." $ \_ -> do
              _ <- Async.async (pure ())
              selected <- Async.async (pure ())
              _ <- Async.wait selected
              pure (StateOutput current, current)
        }
in serveToolsWith 0 tools
