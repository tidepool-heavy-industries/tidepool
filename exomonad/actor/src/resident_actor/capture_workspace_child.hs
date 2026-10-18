do
  let handlerLocal = 51 :: Int
      actualSpec :: AgentSpec CaptureTools '[]
      actualSpec = defaultSpec
        { specTools = CaptureTools
            { ping = presentWith presentDisplay $ tool "Read a value captured by this supplied handler." $ \_ -> pure (handlerLocal + 1)
            , haskell = haskellTool @'Asynchronous @'[] @'[] "Read the retained checkpoint notebook."
            }
        }
  let Right prepared = prepareAgentSpec actualSpec
  started <- send (Core.ForkAgentSpawnWith Core.OpenAIWitness
    (\_ -> installPreparedSpec prepared >> Agents.installRequestReceiver) (Core.CapturedSpawn "CHECKPOINT_TOKEN")
    (Core.ForkDirectory Core.CurrentCheckout) [] (toJSON (Selection.modelPermissionRequests Selection.noModelPermissions)) (Just "CHILD_LABEL")
    (toJSON (Selection.modelSelection (Selection.openAI "capture-catalog" "reader" (Selection.OpenAIOptions Selection.OpenAILow))))
    Nothing CHILD_LIFETIME Nothing (preparedAgentSpecMetadata prepared))
  case started of
    Left failure -> Tidepool.Effects.error (tshow failure) >> pure False
    Right _ -> pure True
