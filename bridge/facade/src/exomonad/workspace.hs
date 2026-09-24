{-# LANGUAGE OverloadedStrings #-}

module Exomonad.Workspace
  ( workspaceIdentity
  , workspaceRoot
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
-- | The project-relative path of the authored workspace directory (the
-- checkout that carries this project's @Project\/*.hs@ and @checks\/@
-- fixtures) — @.exomonad\/workspace@ for a submodule layout, @.exomonad@ for
-- the template layout. Recipe fixture paths are built from this, not
-- hardcoded, so a recipe module works under either layout.
workspaceRoot :: FilePath
workspaceModules :: [Text]
workspacePrompts :: [(Text, Text)]
