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
  started <- send (Core.AgentLaunchSpawnWith
    (Core.CapturedSpawn "CHECKPOINT_TOKEN") (\_ -> installPreparedSpec prepared >> Agents.installRequestReceiver)
    (Core.ForkDirectory Core.CurrentCheckout) [] (Just "CHILD_LABEL")
    Nothing Nothing Nothing CHILD_LIFETIME Nothing (preparedAgentSpecMetadata prepared))
  case started of
    Left failure -> Tidepool.Effects.error (tshow failure) >> pure False
    Right _ -> pure True
