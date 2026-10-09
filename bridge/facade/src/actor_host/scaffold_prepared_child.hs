import qualified AgentSpec as Spec
Right preparedDefaultContext <- checkpoint "prepared default workspace"
Right preparedDefaultChild <- spawnSubagent (ForkCtx preparedDefaultContext) SameDir
  ((defaultSpawnOptions (Spec.agentSpec @'[Replies, Commands, Lookup, Jev, Reflect, BoundWorktree]))
    { spawnLabel = Just "prepared-default-child" })
Right preparedDefaultRequest <- request @Int preparedDefaultChild
  ("return trialSeed plus one" :: Text) defaultRequestOptions
Right preparedDefaultAnswer <- await (result preparedDefaultRequest)
display preparedDefaultAnswer
