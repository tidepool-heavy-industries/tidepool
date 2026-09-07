{-# LANGUAGE OverloadedStrings #-}

module Shoal.Workspace
  ( workspaceIdentity
  , workspaceModules
  , workspacePrompts
  , workspacePrompt
  ) where

import Prelude
import Data.Text (Text)

workspacePrompt :: Text -> Maybe Text
workspacePrompt name = lookup name workspacePrompts

-- The capture owner appends these bindings from the frozen selection.
workspaceIdentity :: Text
workspaceModules :: [Text]
workspacePrompts :: [(Text, Text)]
