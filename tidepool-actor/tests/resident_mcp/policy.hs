let tools :: ResidentTools (AsServerT (Eff ActorEffects))
    tools =
      ResidentTools
        { doubleValue =
            tool "Double one integer." $ \request ->
              pure (EchoOutput (request.value * 2))
        }
in serveTools tools
