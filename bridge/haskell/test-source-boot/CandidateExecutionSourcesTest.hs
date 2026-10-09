module CandidateExecutionSourcesTest (candidateExecutionSourcesTest, executionScopeDescriptorChecks) where

import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm, encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (bracket, evaluate, finally)
import Control.Monad (forM_, unless)
import Data.Maybe (isNothing)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BSL
import Data.Set qualified as Set
import Data.Text qualified as T
import System.Directory (copyFile, createDirectory, listDirectory, removeDirectoryRecursive, removeFile)
import System.FilePath ((</>), takeDirectory, takeFileName)
import System.IO (IOMode(WriteMode), hSetFileSize, withBinaryFile, openBinaryTempFile, hClose)
import System.Timeout (timeout)
import Tidepool.DependencyEvidence
import Tidepool.ExactScope
import Tidepool.ExecutionSource
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), pprAcceptedCandidates, PipelineResult(..)
  , CompilePurpose(..), runPipelineSessionSelected )
import Tidepool.ModuleCandidates
import Tidepool.Session (emptySessionScope, SessionScope(..))
import Tidepool.Test.GenuineCandidate (writeGenuineExecutionScope)
import SourceBootFixtureSupport
  ( withTiming, withScratch, writeExecutionScope, writeManifestFor
  , manifest, preparedNames, hasIntResultLiteral, capturePreparedFixture, captureDiagnostics, digest )

candidateExecutionSourcesTest :: IO ()
candidateExecutionSourcesTest = withTiming $ withScratch $ \work -> do
  forM_ ["MetadataQuoteSupport.hs","MetadataQuoter.hs","ExecutionReexportFacade.hs","ExecutionReexportTarget.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  let source = work </> "ExecutionReexportFacade.hs"
      owners = ["MetadataQuoteSupport","MetadataQuoter"]
      candidatePath = manifest work
      helperName = "MetadataQuoteSupport"
      helperSource = work </> helperName ++ ".hs"
      crossover scopePath = runPipelineSessionSelected (PreparedProducts (Just candidatePath)) Set.empty
        CertifyHomeProductsCompile (Just emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}) source [work] Nothing
      requireImporter scopePath (result,diagnostics) = unless
        (map candidateModule (pprAcceptedCandidates result) == ["MetadataQuoter"]
          && helperName `notElem` preparedNames result) $
        fail ("exact dependency reuse rejected its importer or replaced the protected original"
          ++ "\nretained offer=" ++ candidatePath ++ " scope=" ++ scopePath
          ++ "\naccepted=" ++ show (map candidateOriginalIdentity (pprAcceptedCandidates result))
          ++ " fresh=" ++ show (preparedNames result) ++ "\n" ++ diagnostics)
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing source [work] Nothing
  originalFixture <- capturePreparedFixture work original
  sourceScopePath <- writeExecutionScope work originalFixture ["ExecutionReexportFacade"]
  originalScope <- readExactScope sourceScopePath >>= either fail pure
  emptyExecution <- scopeVariant originalScope (\fields -> take 7 fields ++ [TNull] ++ drop 8 fields)
    >>= either fail pure
  -- Budget policies consume genuinely admitted original owners. Alter only
  -- the graph envelope; no synthetic interface/Core authority is constructed.
  case scopeExecutionGraphs originalScope of
    graph:_ -> do
      let oversized = graph {executionGraphBytes=BS.replicate (executionSourceGraphBytesLimit+1) 0}
          offeredBudget = oversized : tail (scopeExecutionGraphs originalScope)
          references = scopeExecutionOwners originalScope
      bounded <- either (fail . show) pure
        (extendExactExecutionSourcesWithinBudget offeredBudget references emptyExecution)
      unless (isNothing bounded && case extendExactExecutionSources offeredBudget references emptyExecution of
          Left _ -> True; _ -> False) $ fail "optional/advertised aggregate budget policies diverged"
      case scopeExecutionOwners originalScope of
        reference:_ -> unless (case extendExactExecutionSourcesWithinBudget offeredBudget
            [reference {executionRefGraph=replicate 64 'b'}] emptyExecution of Left _ -> True; _ -> False) $
          fail "aggregate budget withholding hid corrupt advertised graph"
        [] -> fail "budget fixture lacks its admitted original references"
    [] -> fail "budget fixture lacks its admitted original graph"
  executionScopeDescriptorChecks sourceScopePath
  writeManifestFor owners work originalFixture
  offered <- readModuleCandidates candidatePath >>= either fail pure
  unless (Set.fromList (map candidateModule offered) == Set.fromList owners
      && all (maybe False (not . null . fst) . candidateExecutionSources) offered) $
    fail "current production candidate issuer lost original execution provenance"
  offerBytes <- BS.readFile candidatePath
  offerTerm <- readTerm candidatePath
  accepted <- runPipelineSessionSelected (PreparedProducts (Just candidatePath)) Set.empty CertifyHomeProductsCompile
    Nothing source [work] Nothing
  unless (Set.fromList (map candidateModule (pprAcceptedCandidates accepted)) == Set.fromList owners) $
    fail "real GHC admission did not accept the proven source-selected originals"
  -- The same production-issued compilation supplies interface closure, native
  -- owner selection and lexical adjacency; no checked-purpose grant is forged.
  crossoverScopePath <- writeGenuineExecutionScope [helperName] [helperName] work originalFixture
  crossoverExact <- readExactScope crossoverScopePath >>= either fail pure
  let helperKey = ("main",helperName)
      helperCandidates = [candidate | candidate <- offered, candidateModule candidate == helperName]
      helperNative = [identity | identity <- scopeExecutionNativeOwners crossoverExact
        , executionIdentityKey identity == helperKey]
  helperCandidate <- case helperCandidates of
    [candidate] -> pure candidate
    _ -> fail "original offer lacks exactly one helper identity"
  let originalHelper = candidateOriginalIdentity helperCandidate
  helperGraphNode <- case candidateExecutionSources helperCandidate of
    Just (graphs,reference) -> either (fail . show) pure
      (executionSourceOriginalNode graphs (executionRefIdentity reference) (executionRefGraph reference))
    Nothing -> fail "original helper candidate lacks its authenticated graph binding"
  importerCandidate <- case [candidate | candidate <- offered, candidateModule candidate == "MetadataQuoter"] of
    [candidate] -> pure candidate
    _ -> fail "original offer lacks exactly one importer"
  importerGraphNode <- case candidateExecutionSources importerCandidate of
    Just (graphs,reference) -> either (fail . show) pure
      (executionSourceOriginalNode graphs (executionRefIdentity reference) (executionRefGraph reference))
    Nothing -> fail "original importer candidate lacks its authenticated graph binding"
  let importerGraph = executionNodeGraph importerGraphNode
      importerHelperOwners = [owner | owner <- executionGraphOwners importerGraph
        , executionIdentityKey (executionOwnerIdentity owner) == helperKey]
      scopeHelperReferences = [reference | reference <- scopeExecutionOwners crossoverExact
        , executionIdentityKey (executionRefIdentity reference) == helperKey]
  unless (helperNative == [originalHelper]
      && executionNodeIdentity helperGraphNode == originalHelper
      && importerHelperOwners == [ExecutionSourceOwner originalHelper True Nothing]
      && map executionRefIdentity scopeHelperReferences == [originalHelper]
      && all (\reference -> executionRefGraph reference `elem`
        map executionGraphSha256 (scopeExecutionGraphs crossoverExact)) scopeHelperReferences) $
    fail ("shared original capture changed its helper native/interface tuple before crossover"
      ++ "\noriginal=" ++ show originalHelper ++ " protected=" ++ show helperNative
      ++ "\nhelper graph=" ++ show (executionGraphSha256 (executionNodeGraph helperGraphNode))
      ++ " importer graph=" ++ show (executionGraphSha256 importerGraph)
      ++ " importer helper owners=" ++ show importerHelperOwners
      ++ " protected references=" ++ show scopeHelperReferences
      ++ "\nretained offer=" ++ candidatePath ++ " scope=" ++ crossoverScopePath)
  putStrLn ("candidate crossover original premise: helper=" ++ show originalHelper
    ++ " importer_graph=" ++ executionGraphSha256 importerGraph
    ++ " owners=" ++ show importerHelperOwners ++ " protected_refs=" ++ show scopeHelperReferences)
  captureDiagnostics (crossover crossoverScopePath) >>= requireImporter crossoverScopePath
  bracket (BS.readFile helperSource <* removeFile helperSource) (BS.writeFile helperSource) $ \_ ->
    captureDiagnostics (crossover crossoverScopePath) >>= requireImporter crossoverScopePath
  protectedRoot <- runPipelineSessionSelected (PreparedProducts (Just candidatePath)) Set.empty
    CertifyHomeProductsCompile Nothing (work </> "MetadataQuoter.hs") [work] Nothing
  unless ("MetadataQuoter" `notElem` map candidateModule (pprAcceptedCandidates protectedRoot)
      && "MetadataQuoter" `elem` preparedNames protectedRoot) $
    fail "fresh compilation target was admitted as a cached replacement root"
  copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" helperSource
  differentOriginal <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing helperSource [work] Nothing
  differentFixture <- capturePreparedFixture work differentOriginal
  mismatchedScopePath <- writeExecutionScope work differentFixture [helperName]
  mismatched <- crossover mismatchedScopePath
  unless (null (pprAcceptedCandidates mismatched) && "MetadataQuoter" `elem` preparedNames mismatched) $
    fail "cached importer admitted a different current exact dependency tuple"
  changed <- runPipelineSessionSelected (PreparedProducts (Just candidatePath)) Set.empty CertifyHomeProductsCompile
    Nothing source [work] Nothing
  unless (null (pprAcceptedCandidates changed)) $
    fail "candidate execution provenance bypassed current source validation"
  copyFile "test-source-boot/fixtures/MetadataQuoteSupport.hs" helperSource
  let parcels = [value | candidate <- pprAcceptedCandidates accepted, Just value <- [candidateExecutionSources candidate]]
  promoted <- either (fail . show) pure
    (extendExactExecutionSources (concatMap fst parcels) (map snd parcels) emptyExecution)
  unless (length (scopeExecutionOwners promoted) == 2
      && scopeLexical promoted == scopeLexical emptyExecution
      && scopeInterfaces promoted == scopeInterfaces emptyExecution) $
    fail "candidate execution promotion changed lexical/interface authority"
  unless (extendExactExecutionSources (concatMap fst parcels) (map snd parcels) promoted == Right promoted) $
    fail "identical original candidate execution promotion conflicts"
  shared <- either (fail . show) pure (executionSourceClosure (scopeExecutionGraphs promoted)
    (scopeExecutionOwners promoted) (scopeExecutionNativeOwners promoted)
    [("main","MetadataQuoter"),("main","MetadataQuoteSupport")])
  unless (length shared == 2) (fail "two roots from one original cycle lost their shared helper")
  helperTarget <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing helperSource [work] Nothing
  helperTargetFixture <- capturePreparedFixture work helperTarget
  helperTargetScopePath <- writeExecutionScope work helperTargetFixture [helperName]
  helperTargetScope <- readExactScope helperTargetScopePath >>= either fail pure
  -- The consumed target retains its native/interface pair. Its generated
  -- input does not grant the independent authored-source replay capability.
  unless (map executionIdentityKey (scopeExecutionNativeOwners helperTargetScope) == [helperKey]
      && null (scopeExecutionOwners helperTargetScope)
      && null (scopeExecutionGraphs helperTargetScope)) $
    fail ("direct helper target changed its native custody or acquired source replay"
      ++ "\nnative=" ++ show (scopeExecutionNativeOwners helperTargetScope)
      ++ " references=" ++ show (scopeExecutionOwners helperTargetScope))
  case executionSourceClosure (scopeExecutionGraphs helperTargetScope)
      (scopeExecutionOwners helperTargetScope) (scopeExecutionNativeOwners helperTargetScope) [helperKey] of
    Left (ExecutionSourceMissing owner) | owner == helperKey -> pure ()
    Left failure -> fail ("direct helper target replay failed at another authority boundary: " ++ show failure)
    Right nodes -> fail ("direct helper target unexpectedly admitted source replay with " ++ show (length nodes) ++ " owners")
  -- Capture an independent compilation in which the helper is an authored
  -- dependency, then select only that helper's native and lexical authority.
  helperOriginal <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "MetadataQuoter.hs") [work] Nothing
  helperFixture <- capturePreparedFixture work helperOriginal
  helperScopePath <- writeGenuineExecutionScope [helperName] [helperName] work helperFixture
  helperScope <- readExactScope helperScopePath >>= either fail pure
  -- The helper's independent receipt has a different graph digest but the
  -- same exact tuple; fresh parent edges retain their own graph provenance.
  helperReference <- case scopeExecutionOwners helperScope of
    [value] -> pure value
    references -> fail ("independent helper dependency cycle lost its sole replay reference: " ++ show references)
  unless (executionRefIdentity helperReference == originalHelper
      && scopeExecutionNativeOwners helperScope == [originalHelper]
      && executionRefGraph helperReference `notElem` map executionGraphSha256 (scopeExecutionGraphs promoted)) $
    fail ("independent helper dependency capture changed its original native/interface tuple or graph"
      ++ "\noriginal=" ++ show originalHelper
      ++ " native=" ++ show (scopeExecutionNativeOwners helperScope)
      ++ " reference=" ++ show helperReference)
  independentlyAuthenticated <- captureDiagnostics (crossover helperScopePath)
  requireImporter helperScopePath independentlyAuthenticated
  let mixedGraphs = scopeExecutionGraphs promoted ++ scopeExecutionGraphs helperScope
      mixedReferences = [if executionIdentityKey (executionRefIdentity reference) == ("main","MetadataQuoteSupport")
        then helperReference else reference | reference <- scopeExecutionOwners promoted]
  differentLocal <- either (fail . show) pure (executionSourceClosure mixedGraphs mixedReferences
    (scopeExecutionNativeOwners promoted) [("main","MetadataQuoter")])
  unless (length differentLocal == 2) (fail "fresh local helper borrowed or required its separately authenticated graph")
  differentShared <- either (fail . show) pure (executionSourceClosure mixedGraphs mixedReferences
    (scopeExecutionNativeOwners promoted) [("main","MetadataQuoter"),("main","MetadataQuoteSupport")])
  unless (length differentShared == 2) (fail "equivalent local recipes from different original cycles did not share their helper")
  let conflictingGraphs = [if executionGraphSha256 graph == executionRefGraph helperReference
        then graph {executionGraphEvidence=(executionGraphEvidence graph) {
          dependencySources=[source {dependencySourceSha256=replicate 64 'f'}
            | source <- dependencySources (executionGraphEvidence graph)]}}
        else graph | graph <- mixedGraphs]
  case executionSourceClosure conflictingGraphs mixedReferences (scopeExecutionNativeOwners promoted)
      [("main","MetadataQuoter"),("main","MetadataQuoteSupport")] of
    Left _ -> pure ()
    Right _ -> fail "two roots silently selected conflicting same-owner original source recipes"
  quoterRef <- case [reference | reference <- scopeExecutionOwners promoted
      , executionIdentityKey (executionRefIdentity reference) == ("main","MetadataQuoter")] of
    [reference] -> pure reference
    _ -> fail "shared-recipe fixture lacks one quoter reference"
  quoterNode <- either (fail . show) pure (executionSourceOriginalNode mixedGraphs
    (executionRefIdentity quoterRef) (executionRefGraph quoterRef))
  let originalGraph = executionNodeGraph quoterNode
      applies row = any (\edge -> dependencyImportQualifier edge == dependencyResolutionQualifier row
        && dependencyImportName edge == dependencyResolutionModule row
        && dependencyImportBoot edge == dependencyResolutionBoot row)
        (dependencyModuleImports (executionNodeModule quoterNode))
      originalEvidence = executionGraphEvidence originalGraph
      alternateGraph = originalGraph {executionGraphSha256=replicate 64 'd',
        executionGraphEvidence=originalEvidence {dependencyResolutions=
          [if applies row then row {dependencyResolutionCandidates=
              (work </> "unproven-shadow.hs") : dependencyResolutionCandidates row}
            else row | row <- dependencyResolutions originalEvidence]}}
  unless (any applies (dependencyResolutions originalEvidence)) $
    fail "shared-recipe fixture lacks applicable negative-resolution witnesses"
  contextualNodes <- either (fail . show) pure (executionSourceOriginalClosure (alternateGraph:mixedGraphs)
    [quoterRef,quoterRef {executionRefGraph=executionGraphSha256 alternateGraph}])
  contextualQuoter <- case [node | node <- contextualNodes
      , executionNodeIdentity node == executionRefIdentity quoterRef] of
    [node] -> pure node
    _ -> fail "shared source recipe lost its exact quoter owner"
  contextualResolutions <- either (fail . show) pure
    (executionNodeOriginalResolutions (alternateGraph:mixedGraphs) contextualQuoter)
  unless (Set.fromList [executionGraphSha256 originalGraph,executionGraphSha256 alternateGraph]
        `Set.isSubsetOf` executionNodeOriginalGraphs contextualQuoter
      && work </> "unproven-shadow.hs" `elem` concatMap dependencyResolutionCandidates contextualResolutions) $
    fail "shared source dedup discarded another recipe's negative-resolution constraints"
  -- Each level shares both later levels. Revalidating settled recipes per
  -- incoming path expands this bounded source inventory exponentially.
  let dagNames = ["SharedRecipe" ++ show index | index <- [0::Int ..35]]
      dagIdentity name = (executionRefIdentity helperReference) {executionModule=name}
      dagPath name = work </> name ++ ".hs"
      dagModules = [DependencyModule "main" name False (dagPath name)
          [DependencyImport DependencyUnqualified child False (Just (dagPath child))
            | child <- take 2 (drop (index+1) dagNames)] ProductReady
        | (index,name) <- zip [0::Int ..] dagNames]
      dagGraph = originalGraph {executionGraphSha256=replicate 64 'c',
        executionGraphOwners=[ExecutionSourceOwner (dagIdentity name) True Nothing | name <- dagNames],
        executionGraphExactImports=[],executionGraphEvidence=originalEvidence {
          dependencySources=[DependencySource (dagPath name) (replicate 64 'a') | name <- dagNames],
          dependencyModules=dagModules,dependencyResolutions=[]}}
      dagRefs=[ExecutionSourceRef (dagIdentity "SharedRecipe0") (executionGraphSha256 dagGraph)]
  dagResult <- timeout 2000000 $ evaluate $ case executionSourceClosure [dagGraph] dagRefs
      (map dagIdentity dagNames) [("main","SharedRecipe0")] of
    Left refusal -> Left refusal
    Right nodes -> Right (length nodes)
  unless (dagResult == Just (Right 36)) $
    fail "shared source recipe DAG did not finish with exactly 36 owners inside its bounded traversal"
  let providerRefs = [reference | (_,reference) <- parcels
        , executionIdentityKey (executionRefIdentity reference) == ("main","MetadataQuoter")]
  local <- either (fail . show) pure
    (extendExactExecutionSources (concatMap fst parcels) providerRefs emptyExecution)
  localNodes <- either (fail . show) pure (executionSourceClosure (scopeExecutionGraphs local)
    (scopeExecutionOwners local) (scopeExecutionNativeOwners local) [("main","MetadataQuoter")])
  unless (length (scopeExecutionOwners local) == 1 && length localNodes == 2) $
    fail "fresh local source recipe incorrectly required a separately published helper capability"
  missingHelper <- scopeVariant emptyExecution (mapNativeRows (filter (\case
    TList (_:TString name:_) -> name /= "MetadataQuoteSupport"
    _ -> True))) >>= either fail pure
  unavailable <- either (fail . show) pure
    (extendExactExecutionSources (concatMap fst parcels) providerRefs missingHelper)
  unless (null (scopeExecutionOwners unavailable) && scopeProducts unavailable == scopeProducts missingHelper
      && case executionSourceClosure (scopeExecutionGraphs unavailable) (scopeExecutionOwners unavailable)
          (scopeExecutionNativeOwners unavailable) [("main","MetadataQuoter")] of Left _ -> True; _ -> False) $
    fail "missing dependency capability either rejected native inventory or authorized an unavailable execution root"
  noProducts <- scopeVariant emptyExecution (mapNativeRows (const [])) >>= either fail pure
  unless (case extendExactExecutionSources (concatMap fst parcels) (map snd parcels) noProducts of Left _ -> True; _ -> False) $
    fail "prospective candidate recipe entered a scope before native promotion"
  let foreignGraphs = [graph {executionGraphProducer=replicate 64 'f'} | graph <- concatMap fst parcels]
  unless (case extendExactExecutionSources foreignGraphs (map snd parcels) emptyExecution of
      Left _ -> True; _ -> False) $ fail "candidate recipe promoted another compiler producer"
  -- Execute the retained production scope through its thin reexport facade.
  -- The typed promotion controls above prove that candidate recipes add no
  -- lexical/interface authority; executable delivery remains the Rust owner's.
  result <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile
    (Just emptySessionScope {ssRoot=work,ssExactScope=Just sourceScopePath})
    (work </> "ExecutionReexportTarget.hs") [work] Nothing
  unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult result))) $
    fail "retained original recipes did not execute through the thin facade"
  -- Negative wire controls mutate a current genuine offer and preserve its
  -- canonical owner evidence. A descriptor promises its exact file bytes.
  originalTerm <- readTerm sourceScopePath
  parcel <- case offerTerm of
    TList [_,_,_,_,_,value,_] -> pure value
    _ -> fail "production candidate offer has another current envelope"
  allOriginals <- case originalTerm of
    TList fields -> pure (fields !! 7)
    _ -> fail "production original scope has another current envelope"
  let envelope value = case offerTerm of
        TList fields -> TList (take 5 fields ++ [value] ++ drop 6 fields)
        _ -> offerTerm
      corrupt = case parcel of
        TList [graphs,TList (TList fields:refs)] ->
          TList [graphs,TList (TList [if index == 2 then TString (T.replicate 64 "f") else field
            | (index,field) <- zip [0::Int ..] fields]:refs)]
        _ -> parcel
      wrongDigest = case parcel of
        TList [TList (TList [_,path]:graphs),refs] ->
          TList [TList (TList [TString (T.replicate 64 "f"),path]:graphs),refs]
        _ -> parcel
      duplicateRef = case parcel of
        TList [graphs,TList (first:refs)] -> TList [graphs,TList (first:first:refs)]
        _ -> parcel
      missingGraph = case parcel of
        TList [_,refs] -> TList [TList [],refs]
        _ -> parcel
  forM_ [("wrong original version",corrupt),("graph digest",wrongDigest)
      ,("duplicate owner",duplicateRef),("unoffered original owner",allOriginals)] $ \(label,invalid) -> do
    writeTerm candidatePath (envelope invalid)
    forM_ [readModuleCandidates candidatePath,
        readModuleCandidatesWithGraphs (scopeExecutionGraphs originalScope) candidatePath] $ \readOffer ->
      readOffer >>= \case
        Left _ -> pure ()
        Right _ -> fail ("candidate execution manifest accepted " ++ label)
  -- Omitting a descriptor differs from promising a missing file: exact-scope
  -- inventory can close the same original reference without widening authority.
  writeTerm candidatePath (envelope missingGraph)
  readModuleCandidates candidatePath >>= \case
    Left _ -> pure ()
    Right _ -> fail "candidate reference resolved without its authenticated original graph"
  sharedOffer <- readModuleCandidatesWithGraphs (scopeExecutionGraphs originalScope) candidatePath
    >>= either fail pure
  -- The reader retains only graphs bundled by this manifest. Scope graphs
  -- close its unchanged references without becoming another local parcel.
  let withoutParcel candidate = candidate {candidateExecutionSource=Nothing}
      graphCustody graphs = Set.fromList
        [(executionGraphSha256 graph,executionGraphBytes graph) | graph <- graphs]
      offeredGraphs = concat [graphs | candidate <- offered
        , Just (graphs,_) <- [candidateExecutionSources candidate]]
  unless (map withoutParcel sharedOffer == map withoutParcel offered) $
    fail "shared exact graph inventory changed a candidate identity, artifact, import or group"
  candidateCustody <- readDescriptorCustody (CandidateGraphFiles candidatePath) parcel
  scopeCustody <- readDescriptorCustody (RetainedScopeGraphFiles sourceScopePath) allOriginals
  unless (graphCustody candidateCustody == graphCustody offeredGraphs
      && graphCustody scopeCustody == graphCustody (scopeExecutionGraphs originalScope)) $
    fail "candidate or scope graph files differ from their authenticated inventory"
  forM_ (zip offered sharedOffer) $ \(bundled,sharedCandidate) ->
    case (candidateExecutionSources bundled,candidateExecutionSources sharedCandidate) of
      (Just (graphs,reference),Just (sharedGraphs,sharedReference)) -> do
        unless (reference == sharedReference && null sharedGraphs) $
          fail ("shared candidate changed its original reference or bundled another graph: "
            ++ show (candidateOriginalIdentity sharedCandidate,sharedReference))
        originalFacts <- originalClosureFacts graphs reference
        sharedFacts <- originalClosureFacts (scopeExecutionGraphs originalScope ++ sharedGraphs) sharedReference
        unless (originalFacts == sharedFacts) $
          fail ("shared candidate changed its authenticated source closure: "
            ++ show (candidateOriginalIdentity sharedCandidate,sharedReference))
      _ -> fail "shared graph fixture lost a candidate's original execution reference"
  BS.writeFile candidatePath offerBytes
  restored <- readModuleCandidates candidatePath >>= either fail pure
  unless (restored == offered) $ fail "restored production offer changed its original execution custody"
  putStrLn "candidate execution sources: current production admission, exact dependency reuse, protected target, source drift refusals, typed promotion and shared-recipe controls, thin reexport execution and current-wire refusals passed"
  where
    readTerm path = do
      bytes <- BS.readFile path
      either (fail . show) (pure . snd) (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes))
    writeTerm path value = BS.writeFile path (toStrictByteString (encodeTerm value))
    readDescriptorCustody files = \case
      TList [descriptors,_] -> do
        decoded <- case deserialiseFromBytes decodeExecutionSourceDescriptors
            (BSL.fromStrict (toStrictByteString (encodeTerm descriptors))) of
          Right (remaining,values) | BSL.null remaining -> pure values
          _ -> fail "issued graph fixture has invalid production descriptors"
        -- The production reader checks transport policy, bounded file bytes and
        -- the complete canonical graph against each advertised digest.
        readExecutionSourceGraphs files [] decoded
      _ -> fail "issued graph fixture has another parcel shape"

originalClosureFacts :: [ExecutionSourceGraph] -> ExecutionSourceRef
  -> IO [(ExecutionSourceIdentity, DependencyModule, String, [(String,String)], Set.Set String
        , [DependencyResolution], String, BS.ByteString)]
originalClosureFacts graphs reference = do
  nodes <- either (fail . show) pure (executionSourceOriginalClosure graphs [reference])
  mapM (\node -> do
    resolutions <- either (fail . show) pure (executionNodeOriginalResolutions graphs node)
    pure (executionNodeIdentity node,executionNodeModule node,executionNodeSourceSha256 node
      ,executionNodeRequirements node,executionNodeOriginalGraphs node,resolutions
      ,executionGraphSha256 (executionNodeGraph node),executionGraphBytes (executionNodeGraph node))) nodes

-- These are decoder refusals around an unchanged producer-issued scope, not
-- new executable authority. Candidate and exact scopes own separate readers.
executionScopeDescriptorChecks :: FilePath -> IO ()
executionScopeDescriptorChecks path = do
  bytes <- BS.readFile path
  original <- readExactScope path >>= either fail pure
  fields <- decode bytes >>= \case
    TList values -> pure values
    _ -> fail "genuine exact scope has another current envelope"
  parcel <- case fields !! 7 of
    TList [TList descriptors,refs] -> pure (descriptors,refs)
    _ -> fail "genuine exact scope has no execution descriptors"
  (sha,graphPath) <- case fst parcel of
    TList [TString sha,TString file]:_ -> pure (sha,T.unpack file)
    _ -> fail "genuine exact scope has no promised graph"
  graphBytes <- BS.readFile graphPath
  unless (digest graphBytes == T.unpack sha
      && scopeRequestSha256 original == digest bytes) $
    fail "genuine exact scope or retained graph differs from its advertised digest"
  originalFacts <- mapM (originalClosureFacts (scopeExecutionGraphs original)) (scopeExecutionOwners original)
  let refuse selectedPath label = readExactScope selectedPath >>= \case
        Left _ -> pure ()
        Right _ -> fail ("exact execution descriptor accepted " ++ label)
      withGraph label change = (change >> refuse path label)
        `finally` BS.writeFile graphPath graphBytes
      withParcelAt selectedPath label value = (BS.writeFile selectedPath
          (toStrictByteString (encodeTerm (TList (take 7 fields ++ [value] ++ drop 8 fields))))
          >> refuse selectedPath label) `finally` BS.writeFile selectedPath bytes
      withParcel = withParcelAt path
      descriptor value = TList [TString sha,value]
      replaceFirst value = TList [TList (value:drop 1 (fst parcel)),snd parcel]
  withGraph "missing graph" (removeFile graphPath)
  withGraph "truncated graph" (BS.writeFile graphPath (BS.take (BS.length graphBytes - 1) graphBytes))
  withGraph "corrupt graph" (BS.writeFile graphPath (BS.singleton 0 <> BS.drop 1 graphBytes))
  withGraph "graph above aggregate bound" $ withBinaryFile graphPath WriteMode $ \handle ->
    hSetFileSize handle (fromIntegral executionSourceGraphBytesLimit + 1)
  withParcel "inline graph bytes" (replaceFirst (descriptor (TBytes graphBytes)))
  withParcel "missing promised graph" (TList [TList [],snd parcel])
  withParcel "duplicate graph" (TList [TList (fst parcel ++ fst parcel),snd parcel])
  -- The child borrows the issuer's retained files; only the bounded manifest
  -- moves. Its complete original references and graph bytes remain unchanged.
  let requestDirectory = path ++ ".request"
  bracket (createDirectory requestDirectory >> pure requestDirectory)
    removeDirectoryRecursive $ \request -> do
      let requestPath = request </> takeFileName path
          readRetained = readExactScope requestPath >>= either fail pure
          requireRetained label = do
            selected <- readRetained
            selectedFacts <- mapM (originalClosureFacts (scopeExecutionGraphs selected)) (scopeExecutionOwners selected)
            entries <- listDirectory request
            selectedBytes <- BS.readFile requestPath
            retainedBytes <- BS.readFile graphPath
            unless (scopeRequestSha256 selected == scopeRequestSha256 original
                && scopeProducerSha256 selected == scopeProducerSha256 original
                && scopeSemanticSha256 selected == scopeSemanticSha256 original
                && scopeInterfaces selected == scopeInterfaces original
                && scopeInterfaceEvidence selected == scopeInterfaceEvidence original
                && scopeLexical selected == scopeLexical original
                && scopeProducts selected == scopeProducts original
                && scopePurpose selected == scopePurpose original
                && scopeRequestTypes selected == scopeRequestTypes original
                && scopeExecutionGraphs selected == scopeExecutionGraphs original
                && scopeExecutionOwners selected == scopeExecutionOwners original
                && selectedFacts == originalFacts
                && selectedBytes == bytes && retainedBytes == graphBytes
                && entries == [takeFileName path]
                && takeDirectory graphPath /= request) $
              fail (label ++ " changed retained path, digest, native owner or original closure")
      BS.writeFile requestPath bytes
      requireRetained "child request"
      -- The exact same external file must still satisfy its original seal.
      (BS.writeFile graphPath (BS.singleton 0 <> BS.drop 1 graphBytes)
          >> refuse requestPath "corrupt retained owner graph")
        `finally` BS.writeFile graphPath graphBytes
      (removeFile graphPath >> refuse requestPath "missing retained owner graph")
        `finally` BS.writeFile graphPath graphBytes
      let wrongSha = if sha == T.replicate 64 "f" then T.replicate 64 "e" else T.replicate 64 "f"
      withParcelAt requestPath "retained graph digest substitution"
        (replaceFirst (TList [TString wrongSha,TString (T.pack graphPath)]))
      withParcelAt requestPath "relative retained graph path"
        (replaceFirst (descriptor (TString (T.pack (takeFileName graphPath)))))
      requireRetained "recovered child request"
  restored <- readExactScope path >>= either fail pure
  unless (not (null (scopeExecutionGraphs restored)) && not (null (scopeExecutionOwners restored))) $
    fail "restored exact scope lost original execution custody"
  unchanged <- BS.readFile path
  unless (unchanged == bytes) $ fail "exact execution descriptor checks changed producer scope"
  where
    decode bytes = either (fail . show) (pure . snd)
      (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes))

-- Variants retain the exact issuer-owned original rows; only admission metadata
-- changes. Invalid wire metadata must be refused before a scope is returned.
scopeVariant :: ExactScope -> ([Term] -> [Term]) -> IO (Either String ExactScope)
scopeVariant original change = do
  bytes <- BS.readFile (scopeManifestPath original)
  fields <- either (fail . show) (pure . snd)
    (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes)) >>= \case
      TList values -> pure values
      _ -> fail "genuine scope variant has another envelope layout"
  (path,handle) <- openBinaryTempFile (takeDirectory (scopeManifestPath original)) "execution-variant.cbor"
  BS.hPut handle (toStrictByteString (encodeTerm (TList (change fields))))
  hClose handle
  readExactScope path

mapNativeRows :: ([Term] -> [Term]) -> [Term] -> [Term]
mapNativeRows change fields = take 6 fields ++ [case fields !! 6 of
  TList rows -> TList (change rows)
  field -> field] ++ drop 7 fields
