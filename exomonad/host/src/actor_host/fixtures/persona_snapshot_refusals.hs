import qualified Tidepool.File as File
import qualified Persona.Memory as Memory
import qualified Tidepool.Agent.Context as C
import Tidepool.Records (FsError(..), WriteOutcome(..))
missingSnapshot <- File.readSnapshot ".exomonad/state/notes/missing-snapshot.md"
directorySnapshot <- File.readSnapshot ".exomonad/state/notes"
missingRevision <- Memory.reviseAt ".exomonad/state/notes/missing-revision.md" (<> "must not create")
Right revision <- Memory.prepareAt ".exomonad/state/notes/file-snapshot-example.md" (<> "stale revision")
Right newer <- Memory.reviseAt ".exomonad/state/notes/file-snapshot-example.md" (<> "newer revision")
Right staleRevision <- Memory.commitRevision revision
Right finalBody <- Files.readFile ".exomonad/state/notes/file-snapshot-example.md"
afterMemoryContext <- C.getContext
display (case (missingSnapshot, directorySnapshot, missingRevision, newer, staleRevision) of
  (Left (FsNotFound _), Left (FsIo _), Left (FsNotFound _), Written {}, WriteConflict {}) ->
    finalBody == "original λ\nnew\nnewer revision" && not (null originalMemoryBlock) && originalMemoryBlock == memoryBlockTexts afterMemoryContext
  _ -> False)
