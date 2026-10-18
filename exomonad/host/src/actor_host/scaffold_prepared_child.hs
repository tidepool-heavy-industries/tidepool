import qualified AgentSpec as Spec
Right preparedDefaultContext <- checkpoint currentModel "prepared default workspace"
Right preparedDefaultChild <- spawnSubagent (ForkCtx preparedDefaultContext) SameDir
  ((defaultSpawnOptions currentModel (Spec.agentSpec @'[Replies, Commands, Lookup, Jev, Reflect, BoundWorktree]))
    { spawnLabel = Just "prepared-default-child" })
Right preparedDefaultRequest <- request @Int preparedDefaultChild
  ("return trialSeed plus one" :: Text) defaultRequestOptions
Right preparedDefaultAnswer <- await (result preparedDefaultRequest)
display preparedDefaultAnswer
