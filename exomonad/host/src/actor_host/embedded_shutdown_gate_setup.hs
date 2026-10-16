data ShutdownGate mode = ShutdownGate
  { shutdownGateState :: mode :- State ()
  , keepShutdownGate :: mode :- Call () NoReply
  } deriving Generic
let shutdownGateDefinition = R.definition "embedded-shutdown-park-gate" (Actor.Selected (knownEffects @'[])) ShutdownGate
      { shutdownGateState = ()
      , keepShutdownGate = \() -> pure ()
      }
shutdownGate <- R.start shutdownGateDefinition
