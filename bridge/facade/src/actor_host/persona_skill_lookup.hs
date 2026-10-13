import qualified Data.Text as T
import qualified Tidepool.Aeson.Value as Json
import qualified Tidepool.Effects.Authored as Files
import qualified Persona.Memory as Memory
import Tidepool.Records (FsError(..))
docs <- LookupApi.lookupRaw (LookupApi.lookupRequest ["doc exomonad-jev", "doc exomonad-agent-work", "doc exomonad-workbench", "doc exomonad-cleanup", "doc exomonad-project-work", "doc exomonad-review", "doc exomonad-command", "doc exomonad-define-actors", "doc exomonad-agent-spec", "doc jev", "doc exomonad-jev/references/recent-changes.md", "doc exomonad-missing", "doc exomonad-jev/../SKILL.md"])
let document r = case LookupApi.lookupOutcome r of { LookupApi.LookupFound [entry] False -> Just (LookupApi.lookupDeclaration entry); _ -> Nothing }
let refused = all (\r -> case LookupApi.lookupOutcome r of { LookupApi.LookupRejected _ -> True; _ -> False }) (drop 11 (LookupApi.lookupResults docs))
writer <- LookupApi.lookupRaw (LookupApi.lookupRequest ["Files.writeFile"])
let writerStatus = case LookupApi.lookupResults writer of { [r] -> case LookupApi.lookupOutcome r of { LookupApi.LookupFound [entry] False -> Just (LookupApi.lookupAvailability entry == LookupApi.LookupAvailable, LookupApi.lookupAvailability entry == LookupApi.LookupUnavailable); _ -> Nothing }; _ -> Nothing }
workspaceEntries <- Files.listDirectory "."
authoredEntries <- Files.listDirectory ".exomonad"
specSource <- Files.readFile ".exomonad/Persona/Spec.hs"
humanMemory <- Memory.recall Memory.Human
insideState <- Files.readFile ".exomonad/state/probe.md"
workspaceSource <- Files.readFile ".agents/skills/exomonad-jev/SKILL.md"
traversal <- Files.readFile "../config.toml"
let workspaceAccessible = (case workspaceEntries of { Right entries -> not (null entries); _ -> False }) && (case authoredEntries of { Right entries -> not (null entries); _ -> False }) && (case specSource of { Right body -> "module Persona.Spec" `T.isInfixOf` body; _ -> False }) && humanMemory == Right "human-memory-original"
let confined = insideState == Right "state marker" && workspaceSource == Right "workspace marker" && (case traversal of { Left (FsSandbox _) -> True; _ -> False })
display (Json.encodeValue (Json.object ["documents" Json..= map (fmap (\body -> (T.length body, T.take 32 body, T.takeEnd 32 body)) . document) (LookupApi.lookupResults docs), "workspace_accessible" Json..= workspaceAccessible, "files_confined" Json..= confined, "docs_refused" Json..= refused, "writer_status" Json..= writerStatus]))
