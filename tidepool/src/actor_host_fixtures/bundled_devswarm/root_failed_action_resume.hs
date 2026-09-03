do
  case sessionInput.rootInterruption of
    Just (AwaitedActorFailed _) -> complete (pure ())
    _ -> error "failed child did not become a typed action interruption"
