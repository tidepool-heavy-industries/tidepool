{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | This workspace's hosted tools: the shell record every actor gets by
-- default, and one tool of our own beside it.
--
-- The declared surface (names, descriptions, argument types, order) is fixed
-- for the life of a session. The bodies are not: edit one, call
-- @reload_agent_spec@, and the next call runs the new code.
module Project.Tools (WorkspaceTools (..), TriageSearch (..), triageSearchBody, tools) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Shell

data TriageSearch = TriageSearch
  { pattern :: Text
  , looking_for :: Text
  }
  deriving (Generic, FromJSON, JsonSchema)

data WorkspaceTools mode = WorkspaceTools
  { shell :: Shell.ShellTools mode
  , triageSearch :: mode :- Call TriageSearch Text
  }
  deriving (Generic)

tools :: Member Cmd.Commands effects => WorkspaceTools (AsServerT (Eff effects))
tools =
  WorkspaceTools
    { shell = Shell.tools
    , triageSearch =
        tool
          "Search the repository, including dotfiles and dotdirectories such as .exomonad/ (but never .git/), for a regular expression and answer with matching files. `pattern` is a ripgrep pattern; `looking_for` is one sentence saying what you hope to find, which the body may use to narrow the answer to the files that matter."
          triageSearchBody
    }

-- | The starting body: every file that matches, with a count, and no judgment
-- about which of them matter. `looking_for` is not used yet, and the answer says
-- so, because the description already promises the judgment a better body makes.
-- `--hidden` so a dotdirectory such as `.exomonad/` (holding this very tool and
-- the rest of the agent spec) is searched too; ripgrep's `--hidden` searches
-- `.git/` right along with it, so `-g '!.git'` excludes that one explicitly.
triageSearchBody :: Member Cmd.Commands effects => TriageSearch -> Eff effects Text
triageSearchBody request = do
  result <-
    Cmd.quiet . Cmd.run $
      Cmd.withArguments [pattern request] (Cmd.bashCommand "rg --hidden -g '!.git' --count-matches --sort path -- \"$1\" || true")
  pure $ case Cmd.stdout result of
    Right found | T.null (T.strip found) -> "no file matches " <> pattern request
    Right found ->
      "unfiltered: this body does not use looking_for yet, so every matching file is listed with its match count.\n"
        <> found
    Left _ -> "the search did not finish cleanly: " <> Cmd.stderr result
