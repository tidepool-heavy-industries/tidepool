{-# LANGUAGE OverloadedStrings #-}
module ForgedAuthoredWorkspaceHandle where
import qualified Tidepool.Effects.Authored as Authored
forged :: Authored.WorkspaceHandle
forged = Authored.WorkspaceHandle "arbitrary-token"
