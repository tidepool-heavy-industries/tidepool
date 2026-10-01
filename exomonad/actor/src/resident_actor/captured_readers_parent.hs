do
  let childEntry :: Int -> Eff '[Core.ActorKernel, Core.AgentTools] ()
      childEntry _ = do
        send Core.ActorReadyWith
        serveToolsWith () $ \_ -> CaptureTools
          { ping = tool "Read the completed private capture." $ \_ -> pure (capturedValue + 1) }
      startChild group path = send (Core.ForksStartWith
        path childEntry Nothing group
        Core.ActorResearchRole Core.ActorReadOnlyProfile []
        Nothing Core.RequireClean [] Nothing Nothing Nothing
        Core.InheritedContext (Just "CHECKPOINT_TOKEN") Nothing Core.ParentOwned)
  begun <- send (Core.ForksBeginWith False "captured/readers" ["first", "second"])
  case begun of
    Right (group, _, [firstPath, secondPath]) -> do
      first <- startChild group firstPath
      second <- startChild group secondPath
      case (first, second) of
        (Right _, Right _) -> do
          committed <- send (Core.ForksCommitCapturedWith group)
          case committed of
            Right () -> do
              barrier <- send (Core.ForksBeginWith False "captured/failure-barrier" ["blocker"])
              case barrier of
                Right (barrierGroup, _, [blockerPath]) -> do
                  blocked <- startChild barrierGroup blockerPath
                  case blocked of
                    Left refusal -> error (tshow refusal) >> pure False
                    Right _ -> error "failure barrier unexpectedly admitted" >> pure False
                _ -> error "failure barrier group admission failed" >> pure False
            Left refusal -> error (tshow refusal) >> pure False
        _ -> error "captured reader launch failed" >> pure False
    _ -> error "captured group admission failed" >> pure False
