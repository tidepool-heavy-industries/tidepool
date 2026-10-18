import qualified Tidepool.Actors.Spawn as Spawn
do
  let validSpec :: AgentSpec CaptureSyncTools '[]
      validSpec = defaultSpec { specTools = CaptureSyncTools
        { syncPing = presentWith presentDisplay (syncTool "Object input" (\_ -> pure (1 :: Int))) } }
      invalidSyncSpec :: AgentSpec InvalidCaptureSyncTools '[]
      invalidSyncSpec = defaultSpec { specTools = InvalidCaptureSyncTools
        { scalarSyncPing = presentWith presentDisplay (syncTool "Invalid scalar input" (\_ -> pure (2 :: Int))) } }
      invalidSpec :: AgentSpec InvalidCaptureTools '[]
      invalidSpec = defaultSpec { specTools = InvalidCaptureTools
        { scalarPing = presentWith presentDisplay (tool "Invalid scalar function input" (\_ -> pure (2 :: Int))) } }
      Right validPrepared = prepareAgentSpec validSpec
      Right invalidPrepared = prepareAgentSpec invalidSpec
  checked <- Spawn.validateAgentSpec validSpec
  case checked of
    Right _ -> pure ()
    Left failure -> Tidepool.Effects.error ("environment-owned intrinsic support missing: " <> tshow failure)
  rejected <- Spawn.validateAgentSpec invalidSyncSpec
  case rejected of
    Left (Core.SpecInputSchemaInvalid _) -> pure ()
    other -> Tidepool.Effects.error ("expected object schema refusal after intrinsic support admission: " <> tshow (either Left (const (Right ())) other))
  started <- send (Core.ForkAgentSpawnWith Core.OpenAIWitness
    (\_ -> installPreparedSpec invalidPrepared >> Agents.installRequestReceiver)
    (Core.CapturedSpawn "CHECKPOINT_TOKEN")
    (Core.ForkDirectory Core.CurrentCheckout) [] (toJSON ([] :: [Text])) (Just "forged-spec-child")
    (toJSON (Selection.modelSelection (Selection.openAI "capture-catalog" "reader" (Selection.OpenAIOptions Selection.OpenAILow))))
    Nothing Core.ActorOwned Nothing
    (preparedAgentSpecMetadata validPrepared))
  case started of
    Left failure@(Core.SpawnPartialFailure (Core.SpawnRetainedActor _ _) Core.SpawnCleanupConfirmed _) -> pure (tshow failure)
    other -> Tidepool.Effects.error ("expected retained actor with confirmed failed-install cleanup: " <> tshow other) >> pure "unexpected"
