{-# LANGUAGE TypeApplications #-}
import qualified Persona.Spec as Persona
Right progressActor <- spawnSubagent (FreshCtx "Return the requested integer without changing shared files.") SameDir
  ((defaultSpawnOptions Persona.helperSpec) { spawnLabel = Just "reload-progress", spawnLifetime = ActorOwned })
Right progressReply <- request @Int progressActor (73 :: Int) defaultRequestOptions
display True
