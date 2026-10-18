{-# LANGUAGE TypeApplications #-}
import qualified Persona.Spec as Persona
import qualified Tidepool.Actors.Exomonad as Actors
Right progressActor <- spawnSubagent (FreshCtx "Return the requested integer without changing shared files.") SameDir
  ((defaultSpawnOptions currentModel Persona.helperSpec) { spawnLabel = Just "reload-progress", spawnLifetime = ActorOwned })
Right progressReply <- Actors.request @Int progressActor (73 :: Int) defaultRequestOptions
display True
