import qualified Tidepool.Agent.Context as C

do
  let note = C.appendNote "independent-note-roundtrip" []
  C.ContextStaged _ <- C.stageContext (C.contextPatch [note, note])
  pure True
