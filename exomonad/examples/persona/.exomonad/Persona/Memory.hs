{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Persona.Memory (Block(..), path, remember, recall, initialise, note, requestChange) where
import Prelude hiding (readFile, writeFile)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Effects.Authored
  (FsRead, FsWrite, readFile, writeFile, writeCheckedIf)
import Tidepool.Records (FsError(..), WriteOutcome(..))

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
remember :: Member FsWrite effects => Block -> Text -> Eff effects (Either FsError ())
remember block = writeFile (path block)
-- Create-only publication makes reinitialisation preserve an evolving persona.
-- Storage errors are never interpreted as absence.
initialise :: Member FsWrite effects => Eff effects [Either FsError WriteOutcome]
initialise = traverse (\block -> writeCheckedIf Nothing (path block) (seed block)) [Persona, Human, Curiosities]
-- Names are single components within the persistent notes and change directories.
note :: Member FsWrite effects => Text -> Text -> Eff effects (Either FsError ())
note name body = case component name of
  Left err -> pure (Left err)
  Right file -> writeFile (".exomonad/state/notes/" <> file <> ".md") body
requestChange :: Member FsWrite effects => Text -> Text -> Text -> Eff effects (Either FsError ())
requestChange name activeRevision proposal = case component name of
  Left err -> pure (Left err)
  Right file -> writeFile (".exomonad/state/changes/" <> file <> ".md")
    ("# Proposed harness change\n\nActive source revision: " <> activeRevision <> "\n\n" <> proposal <>
     "\n\nThis is a request for the operator's coding session. It has not changed the running harness.\n")
component :: Text -> Either FsError Text
component name
  | T.null name || name == "." || name == ".." || T.any (`elem` ("/\\\0" :: String)) name = Left (FsSandbox "Use a nonempty single-component note name")
  | otherwise = Right name
