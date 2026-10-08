{-# LANGUAGE OverloadedStrings #-}
module ForgedWorkspaceHandle where
import qualified Tidepool.Agent.Launch as Launch
forged :: Launch.WorkspaceHandle
forged = Launch.WorkspaceHandle "arbitrary-token"
