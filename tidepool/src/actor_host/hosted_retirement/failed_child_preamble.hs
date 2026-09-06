type ActorEffects = '[AgentTools, Actor]
data SpawnInput = SpawnInput { seed :: Int } deriving (Generic, FromJSON, JsonSchema)
data SpawnOutput = SpawnOutput { started :: Bool } deriving (Generic, ToJSON, JsonSchema)
data ResidentTools mode = ResidentTools { spawnChild :: mode :- Call SpawnInput SpawnOutput } deriving (Generic)
