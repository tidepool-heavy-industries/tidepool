
let idleDefinition :: ActorDefinition Int Maybe Int
    idleDefinition =
      ActorDefinition
        { label = "idle-worker"
        , effectProfile = ReadWrite
        , initialization = \seed -> pure seed
        , behavior = \_seed initial ->
            (pure initial :: Eff (ReadWriteEffects Maybe) Int)
        , visibleToChild = []
        , onShutdown = \reason -> case reason of
            ShutdownCompleted -> deliberate "completed shutdown must not deliberate" ()
            _ -> pure ()
        }

    workerDefinition :: ActorDefinition Int Maybe Int
    workerDefinition =
      ActorDefinition
        { label = "worker"
        , effectProfile = ReadOnly
        , initialization = \seed -> do
            approved <- deliberate "Approve the supplied seed." seed
            adjustment <- deliberate "Choose the adjustment." seed
            pure (approved, adjustment)
        , behavior = \seed (approved, adjustment) ->
            (pure (if approved then seed + adjustment else seed - adjustment)
              :: Eff (ReadOnlyEffects Maybe) Int)
        , visibleToChild = []
        , onShutdown = const (pure ())
        }

    serverDefinition :: ActorDefinition Int ((,) Int) Int
    serverDefinition =
      ActorDefinition
        { label = "server"
        , effectProfile = ReadOnly
        , initialization = \seed -> pure seed
        , behavior = \_seed initial ->
            (serve initial (\state (delta, result) -> pure (result, state + delta))
              :: Eff (ReadOnlyEffects ((,) Int)) Int)
        , visibleToChild = []
        , onShutdown = \reason -> case reason of
            ShutdownCancelled -> deliberate "shutdown must not deliberate" ()
            _ -> pure ()
        }

    jobDefinition :: ActorDefinition Int ((,) Int) Int
    jobDefinition =
      ActorDefinition
        { label = "job"
        , effectProfile = ReadOnly
        , initialization = \seed -> pure seed
        , behavior = \_seed initial ->
            (receive (\(delta, result) -> pure (result, initial + delta))
              :: Eff (ReadOnlyEffects ((,) Int)) Int)
        , visibleToChild = []
        , onShutdown = const (pure ())
        }

in do
    dynamicProgram <-
      (deliberate "Define a typed child actor." ()
        :: Eff ActorEffects (Eff ActorEffects Int))
    dynamicAnswer <- dynamicProgram
    _ <- startActor idleDefinition 10
    _ <- startActor workerDefinition 41
    server <- startActor serverDefinition 0
    cast server (1, ())
    serverAnswer <- call server (2, 41)
    job <- startActor jobDefinition 10
    jobAnswer <- call job (5, 42)
    jobExit <- awaitExit job
    case jobExit of
      Completed value ->
        pure (dynamicAnswer, serverAnswer, jobAnswer, value)
      _ -> pure (-1, -1, -1, -1)
