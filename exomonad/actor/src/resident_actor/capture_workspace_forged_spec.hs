do
  let validSpec :: AgentSpec CaptureTools '[]
      validSpec = defaultSpec { specTools = CaptureTools
        { ping = presentWith presentDisplay (tool "Object input" (\_ -> pure (1 :: Int)))
        , haskell = haskellTool @'Asynchronous @'[] @'[] "Retained notebook"
        } }
      invalidSpec :: AgentSpec InvalidCaptureTools '[]
      invalidSpec = defaultSpec { specTools = InvalidCaptureTools
        { scalarPing = presentWith presentDisplay (tool "Invalid scalar function input" (\_ -> pure (2 :: Int))) } }
      Right validPrepared = prepareAgentSpec validSpec
      Right invalidPrepared = prepareAgentSpec invalidSpec
  started <- send (Core.AgentLaunchSpawnWith
    (Core.CapturedSpawn "CHECKPOINT_TOKEN")
    (\_ -> installPreparedSpec invalidPrepared >> Agents.installRequestReceiver)
    (Core.ForkDirectory Core.CurrentCheckout) [] (Just "forged-spec-child")
    Nothing Nothing Nothing Core.ActorOwned Nothing
    (preparedAgentSpecMetadata validPrepared))
  case started of
    Left failure@(Core.SpawnPartialFailure (Core.SpawnRetainedActor _ _) Core.SpawnCleanupConfirmed _) -> pure (tshow failure)
    other -> Tidepool.Effects.error ("expected retained actor with confirmed failed-install cleanup: " <> tshow other) >> pure "unexpected"
