{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
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
import Control.Monad (forM)
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Shell
import Tidepool.Effects.Core (Jev)
import qualified Jev.Operators as J
import Jev.Operators (Packet ((:=), (:&)))

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

tools :: (Member Cmd.Commands effects, Member Jev effects) => WorkspaceTools (AsServerT (Eff effects))
tools =
  WorkspaceTools
    { shell = Shell.tools
    , triageSearch =
        tool
          "Search the repository, including dotfiles and dotdirectories such as .shoal/ (but never .git/), for a regular expression and answer with matching files. `pattern` is a ripgrep pattern; `looking_for` is one sentence saying what you hope to find, which the body may use to narrow the answer to the files that matter."
          triageSearchBody
    }

-- | Rank a bounded candidate set from short matching excerpts. Preserve every
-- match in the answer so a weak judgment cannot hide the owning source.
triageSearchBody :: (Member Cmd.Commands effects, Member Jev effects) => TriageSearch -> Eff effects Text
triageSearchBody request = do
  result <-
    Cmd.quiet . Cmd.run $
      Cmd.withArguments [pattern request] (Cmd.bashCommand "rg --hidden -g '!.git' --count-matches --sort path -- \"$1\"; code=$?; if [ \"$code\" -eq 1 ]; then exit 0; else exit \"$code\"; fi")
  case Cmd.stdout result of
    Right found | T.null (T.strip found) -> pure ("no file matches " <> pattern request)
    Right found | length (T.lines found) > 40 ->
      pure ("unfiltered: more than 40 candidates; narrow the regex for semantic triage.\n" <> found)
    Right found -> do
      filesResult <-
        Cmd.quiet . Cmd.run $
          Cmd.withArguments [pattern request]
            (Cmd.bashCommand "rg --hidden -g '!.git' --files-with-matches --sort path -- \"$1\"")
      case Cmd.stdout filesResult of
        Left issue -> pure ("unfiltered: candidate preview search failed: " <> T.pack (show issue) <> "\n" <> found)
        Right files -> do
          candidates <- forM (T.lines files) $ \path -> do
            preview <-
              Cmd.quiet . Cmd.run $
                Cmd.withArguments [pattern request, path]
                  (Cmd.bashCommand "rg --line-number --max-count 3 --context 2 --max-columns 240 --max-columns-preview -- \"$1\" \"$2\"")
            pure (path, either (const "preview unavailable") id (Cmd.stdout preview))
          answer <- J.ask
            (J.state (#intent := looking_for request :& #pattern := pattern request))
            (#candidates := J.each fst (\(path, preview) ->
              #relevant := J.noul
                ("Does this bounded matching excerpt contain evidence that the file is promising to read for the stated intent? "
                  <> "Judge only the excerpt; do not assume unseen contents.\nFile: "
                  <> path <> "\nExcerpt:\n" <> preview)) candidates)
          pure $ case answer of
            Left err -> "unfiltered: Jev unavailable: " <> T.pack (show err) <> "\n" <> found
            Right judged ->
              let promising = [path | ((path, _), a) <- judged.candidates, a.relevant.yes >= 0.7]
              in "Content-based reading suggestions from bounded excerpts:\n"
                 <> T.unlines promising <> "All matches with counts (retained):\n" <> found
    Left issue -> pure ("the search did not finish cleanly: " <> T.pack (show issue) <> "\n" <> Cmd.stderr result)
