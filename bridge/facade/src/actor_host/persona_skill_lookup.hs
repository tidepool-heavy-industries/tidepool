import qualified Tidepool.Aeson.Value as Json
import qualified Tidepool.Effects.Authored as Files
import Tidepool.Records (FsError(..))
docs <- LookupApi.lookupRaw (LookupApi.lookupRequest ["doc exomonad-jev", "doc exomonad-agent-work", "doc exomonad-workbench", "doc exomonad-cleanup", "doc exomonad-project-work", "doc exomonad-review", "doc exomonad-command", "doc exomonad-define-actors", "doc exomonad-agent-spec", "doc jev", "doc exomonad-jev/references/recent-changes.md", "doc exomonad-missing", "doc exomonad-jev/../SKILL.md"])
let document r = case LookupApi.lookupOutcome r of { LookupApi.LookupFound [entry] False -> Just (LookupApi.lookupDeclaration entry); _ -> Nothing }
insideState <- Files.readFile "probe.md"
outsideState <- Files.readFile ".agents/skills/exomonad-jev/SKILL.md"
traversal <- Files.readFile "../config.toml"
let confined = insideState == Right "state marker" && (case outsideState of { Left (FsNotFound _) -> True; _ -> False }) && (case traversal of { Left (FsSandbox _) -> True; _ -> False })
display (Json.encodeValue (Json.object ["documents" Json..= map (fmap (\body -> (T.length body, T.take 256 body)) . document) (LookupApi.lookupResults docs), "files_confined" Json..= confined]))
