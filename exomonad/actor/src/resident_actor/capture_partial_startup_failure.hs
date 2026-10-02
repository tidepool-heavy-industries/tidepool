do
  let childEntry :: Int -> Eff '[Core.ActorKernel, Core.AgentTools] ()
      childEntry _ = do
        send Core.ActorReadyWith
        serveToolsWith () $ \_ -> CaptureTools
          { ping = tool "Read the completed capture." $ \_ -> pure (capturedValue + 1) }
      startChild group path = send (Core.ForksStartWith
        path childEntry Nothing group
        Core.ActorResearchRole Core.ActorReadOnlyProfile []
        Nothing Core.RequireClean [] Nothing Nothing Nothing
        Core.InheritedContext (Just "CHECKPOINT_TOKEN") Nothing Core.ActorOwned)
  begun <- send (Core.ForksBeginWith False "captured/partial-startup" ["first", "second"])
  case begun of
    Right (group, _, [firstPath, secondPath]) -> do
      first <- startChild group firstPath
      case first of
        Right _ -> do
          second <- startChild group secondPath
          case second of
            Left refusal -> error ("expected partial child startup refusal: " <> tshow refusal) >> pure False
            Right _ -> error "second partial child unexpectedly started" >> pure False
        Left refusal -> error ("first partial child did not start: " <> tshow refusal) >> pure False
    _ -> error "partial group admission failed" >> pure False
