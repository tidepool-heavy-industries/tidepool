{-# LANGUAGE OverloadedStrings #-}
module ForgedContextCheckpoint where
import qualified Tidepool.Actors.Spawn as Spawn
forged :: Spawn.ContextCheckpoint
forged = Spawn.ContextCheckpoint "arbitrary-token"
