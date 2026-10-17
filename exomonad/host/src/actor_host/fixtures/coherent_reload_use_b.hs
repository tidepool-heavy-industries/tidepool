import qualified Project.Work as Work
import qualified Tidepool.Lookup as L
import qualified Tidepool.Effects.Authored as Files
import qualified Tidepool.Effects.Core as Effects
import qualified Data.Text as Text

coherentLookupDiagnostic :: L.LookupResult -> (Text.Text, Text.Text, Bool)
coherentLookupDiagnostic result =
  let identities entries = map (\entry -> (L.lookupModule entry, L.lookupName entry, L.lookupKind entry, L.lookupAvailability entry, L.lookupOrigin entry)) (take 8 entries)
  in case L.lookupOutcome result of
    L.LookupFound entries clipped -> ("LookupFound", Text.pack (show (identities entries)), clipped)
    L.LookupAmbiguous entries clipped -> ("LookupAmbiguous", Text.pack (show (identities entries)), clipped)
    L.LookupMissing searched suggestions -> ("LookupMissing", Text.pack (show (take 8 searched, take 8 suggestions)), False)
    L.LookupRejected reason -> ("LookupRejected", Text.take 512 reason, False)

bOnly <- pure (Work.readStamp (Work.Stamp 20 22))
missing <- L.lookupRaw (L.lookupRequest ["aOnly", "aLate"])
Right workBExpected <- Files.readFile "reload-inputs/work-b.txt"
Right (runningBRevision, runningB) <- readSourceModule "Project.Work"
Right coherentSourceStatus <- Files.sourceStatus
display (Text.unlines
  [ "coherent-use-b constituents"
  , "bOnly=" <> Text.pack (show bOnly)
  , "answerFn1=" <> Text.pack (show (Work.answerFn 1))
  , "sourceBodiesEqual=" <> Text.pack (show (runningB == workBExpected))
  , "readSourceRevision=" <> runningBRevision
  , "activeRevision=" <> Effects.revisionIdentity (Effects.statusActive coherentSourceStatus)
  , "diskRevision=" <> Effects.revisionIdentity (Effects.statusDisk coherentSourceStatus)
  , "activeWorkDigests=" <> Text.pack (show [(Effects.sourceModuleName source, Effects.sourceModuleDigest source) | source <- Effects.revisionModules (Effects.statusActive coherentSourceStatus), Effects.sourceModuleName source == "Project.Work"])
  , "diskWorkDigests=" <> Text.pack (show [(Effects.sourceModuleName source, Effects.sourceModuleDigest source) | source <- Effects.revisionModules (Effects.statusDisk coherentSourceStatus), Effects.sourceModuleName source == "Project.Work"])
  , "lookupOutcomes=" <> Text.pack (show [(L.lookupQuery result, coherentLookupDiagnostic result) | result <- take 8 (L.lookupResults missing)])
  , "lookupCandidateReferences=" <> Text.pack (show [(L.candidateQuery candidate, L.candidateReference candidate) | candidate <- take 8 (L.lookupCandidates missing)])
  , "lookupCount=" <> Text.pack (show (length (L.lookupResults missing)))
  ])
display (bOnly == 42 && Work.answerFn 1 == 42 && runningB == workBExpected
  && all (\r -> case L.lookupOutcome r of { L.LookupMissing _ _ -> True; _ -> False }) (L.lookupResults missing) && length (L.lookupResults missing) == 2)
