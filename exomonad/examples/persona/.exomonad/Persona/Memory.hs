{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Persona.Memory
  ( Block(..), path, recall, replaceBlock, revise
  , Revision, prepareAt, commitRevision, reviseAt, initialise, note
  ) where
import Prelude hiding (readFile, writeFile)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Effects.Authored
  (FsRead, FsWrite, readFile, writeFile, writeCheckedIf)
import Tidepool.Records (FsError(..), WriteOutcome(..))
import qualified Tidepool.File as File

data Block = Persona | Human | Curiosities deriving (Eq, Show)
path :: Block -> Text
path Persona = ".exomonad/state/memory/persona.md"
path Human = ".exomonad/state/memory/human.md"
path Curiosities = ".exomonad/state/memory/curiosities.md"
seed :: Block -> Text
seed Persona = "# Persona\n\nAn open beginning. Character, interests and ways of working may grow through conversation and use.\n"
seed Human = "# Human\n\nAn open beginning. Preserve the human’s steering and corrections as you learn one another.\n"
seed Curiosities = "# Curiosities\n\nAn open beginning. Keep questions and discoveries worth returning to; interests need no practical destination.\n"
recall :: Member FsRead effects => Block -> Eff effects (Either FsError Text)
recall = readFile . path
-- Whole-block replacement is explicit; ordinary edits use revise.
replaceBlock :: Member FsWrite effects => Block -> Text -> Eff effects (Either FsError ())
replaceBlock block = writeFile (path block)

data Revision = Revision File.FileSnapshot Text

-- Capture the contents, version and destination together. Missing files and
-- storage errors remain errors; preparing a revision never creates a block.
prepareAt :: Member FsRead effects
  => Text -> (Text -> Text) -> Eff effects (Either FsError Revision)
prepareAt target edit = fmap propose <$> File.readSnapshot target
  where propose snapshot = Revision snapshot (edit (File.snapshotText snapshot))

commitRevision :: Member FsWrite effects
  => Revision -> Eff effects (Either FsError WriteOutcome)
commitRevision (Revision snapshot body) = File.writeSnapshot snapshot body

reviseAt :: (Member FsRead effects, Member FsWrite effects)
  => Text -> (Text -> Text) -> Eff effects (Either FsError WriteOutcome)
reviseAt target edit = do
  proposal <- prepareAt target edit
  either (pure . Left) commitRevision proposal

-- Saving a revision never refreshes current context and never retries a
-- conflict. The caller can inspect current contents and choose a new edit.
revise :: (Member FsRead effects, Member FsWrite effects)
  => Block -> (Text -> Text) -> Eff effects (Either FsError WriteOutcome)
revise block = reviseAt (path block)
-- Create-only publication makes reinitialisation preserve an evolving persona.
-- Storage errors are never interpreted as absence.
initialise :: Member FsWrite effects => Eff effects [Either FsError WriteOutcome]
initialise = traverse (\block -> writeCheckedIf Nothing (path block) (seed block)) [Persona, Human, Curiosities]
-- Names are single components within the persistent notes and change directories.
note :: Member FsWrite effects => Text -> Text -> Eff effects (Either FsError ())
note name body = case component name of
  Left err -> pure (Left err)
  Right file -> writeFile (".exomonad/state/notes/" <> file <> ".md") body
component :: Text -> Either FsError Text
component name
  | T.null name || name == "." || name == ".." || T.any (`elem` ("/\\\0" :: String)) name = Left (FsSandbox "Use a nonempty single-component note name")
  | otherwise = Right name
