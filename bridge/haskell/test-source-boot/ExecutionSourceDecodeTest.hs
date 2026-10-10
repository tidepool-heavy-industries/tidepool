module ExecutionSourceDecodeTest (executionSourceDecodeChecks, executionSourceDecodeBenchmark, executionSourceDecodeSnapshots, executionSourceResolutionBudgetChecks) where

import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Decoding (Decoder)
import Codec.CBOR.Term (Term(..), decodeTerm, encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, bracket, evaluate, try)
import Control.Monad (forM_, unless, when)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.Char (ord)
import Data.IORef (newIORef, readIORef)
import Data.List (isInfixOf)
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Text.Encoding qualified as TE
import GHC.Clock (getMonotonicTimeNSec)
import GHC.Stats (RTSStats(..), getRTSStats, getRTSStatsEnabled)
import System.CPUTime (getCPUTime)
import System.Directory (createDirectoryIfMissing, getTemporaryDirectory, removeFile, removePathForcibly)
import System.FilePath ((</>))
import System.IO (hClose, openBinaryTempFile)
import System.Mem (performGC)
import Text.Read (readMaybe)
import Tidepool.DependencyEvidence
import Tidepool.ExecutionSource
import Tidepool.ExtractUtil (shaHex)

-- Start with bytes issued by the production owner; mutate only the selected
-- CBOR inventory/field so failures exercise the real admission decoder.
fixtureBytes :: IO BS.ByteString
fixtureBytes = do
  let sha = replicate 64 'a'
      source = "module Expr where\nanswer = 42\n"
      original name = ExecutionSourceIdentity "main" name sha sha sha
      evidence = DependencyEvidence True True
        [DependencySource "@generated-source" (shaHex (TE.encodeUtf8 (T.pack source))),
         DependencySource "/decoder-fixture/Support.hs" sha]
        [] [] [DependencyModule "main" "Expr" False "@generated-source" [] ProductReady,
               DependencyModule "main" "Support" False "/decoder-fixture/Support.hs" [] ProductReady]
      recipe = ExecutionSourceRecipe sha Nothing [] ("/decoder-fixture/Expr.hs", source)
        evidence [ExecutionSourceOwner (original "Expr") True Nothing,
                  ExecutionSourceOwner (original "Support") True Nothing]
        [(("main", "Expr"), [("main", "Support")])]
        [("pkg", "Package", "/decoder-fixture/Package.hi", sha)]
  case issueExecutionSourceRecipe recipe of
    Right (Just graph) -> pure (executionGraphBytes graph)
    result -> fail ("decoder fixture issuance failed: " ++ show result)

at :: Int -> (Term -> Term) -> Term -> Term
at index change (TList values) = TList
  [if position == index then change value else value | (position, value) <- zip [0..] values]
at _ _ _ = error "decoder fixture has a non-list row"

duplicate :: Term -> Term
duplicate (TList (first : rest)) = TList (first : first : rest)
duplicate _ = error "decoder fixture has an empty inventory"

encode :: Term -> BS.ByteString
encode = toStrictByteString . encodeTerm

decode :: BS.ByteString -> Either String ExecutionSourceGraph
decode bytes = decodeExecutionSourceGraph (shaHex bytes) bytes

expectParcelRejection :: String -> (forall state. Decoder state value) -> BS.ByteString -> IO ()
expectParcelRejection label decoder encoded = case deserialiseFromBytes decoder (BL.fromStrict encoded) of
  Left actual | label `isInfixOf` show actual -> pure ()
  _ -> fail ("decoder lost duplicate gate: " ++ label)

executionSourceDecodeChecks :: IO ()
executionSourceDecodeChecks = do
  bytes <- fixtureBytes
  term <- case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
    Right (remaining, value) | BL.null remaining -> pure value
    _ -> fail "issued decoder fixture has malformed CBOR"
  graph <- either fail pure (decode bytes)
  unless (executionGraphBytes graph == bytes) (fail "decoder changed graph bytes")
  executionSourceFileCustodyChecks graph
  inheritedIdentityChecks graph
  contextualDependencyChecks
  let refuse label reason value = case decode (encode value) of
        Left actual | reason `isInfixOf` actual -> pure ()
        result -> fail (label ++ ": unexpected decoder result " ++ show result)
      accept label value = either (fail . ((label ++ ": ") ++))
        (\decoded -> evaluate (forceGraph decoded) >> pure ()) (decode (encode value))
      include value = at 5 (const (TList [TString value])) term
      unit value = at 8 (at 0 (at 0 (const (TString value)))) term
      source value = at 6 (at 1 (const (TString value))) term
      repeated count = at 7 (at 2 (const (TList
        [TList [TString (T.pack ("/decoder-fixture/" ++ show index)), TString (T.replicate 64 "a")]
        | index <- [1..count :: Int]]))) term
  forM_
    [ ("sources", "source paths", at 7 (at 2 duplicate) term)
    , ("modules", "module owners", at 7 (at 4 duplicate) term)
    , ("native", "native owners", at 8 duplicate term)
    , ("exact owners", "exact owners", at 9 duplicate term)
    , ("exact imports", "exact imports", at 9 (at 0 (at 2 duplicate)) term)
    , ("packages", "package owners", at 10 duplicate term)
    ] $ \(label, kind, value) -> refuse label ("duplicate original execution " ++ kind) value
  accept "4096 source rows" (repeated 4096)
  refuse "4097 source rows" "original execution inventory exceeds bound" (repeated 4097)
  accept "65536 ASCII owner characters" (unit (T.replicate 65536 "x"))
  refuse "65537 ASCII owner characters" "invalid original execution owner" (unit (T.replicate 65537 "x"))
  accept "65536 astral owner characters" (unit (T.replicate 65536 "\x1f642"))
  refuse "65537 astral owner characters" "invalid original execution owner" (unit (T.replicate 65537 "\x1f642"))
  accept "65536 Unicode path characters" (include ("/" <> T.replicate 65535 "\x1f642"))
  refuse "empty path precedes absolute check" "invalid original execution owner" (include "")
  refuse "relative path after owner check" "relative original execution path" (include "relative")
  refuse "text limit precedes owner limit" "original execution text exceeds bound"
    (unit (T.replicate (4 * 1024 * 1024 + 1) "x"))
  accept "source text at character limit" (source (T.replicate (4 * 1024 * 1024) "x"))
  accept "source text above metadata bound"
    (source (T.replicate (4 * 1024 * 1024 + 1) "x"))
  case decodeExecutionSourceGraph (replicate 64 '0') bytes of
    Left "original execution graph digest differs" -> pure ()
    _ -> fail "decoder lost digest rejection precedence"
  let sha = T.pack (shaHex bytes)
      descriptor = TList [TString sha, TString "/decoder-fixture/execution.cbor"]
      parcel = encode (TList [descriptor, descriptor])
      reference = TList [TString "main", TString "Support", TString (T.replicate 64 "a"),
        TString (T.replicate 64 "a"), TString (T.replicate 64 "a"), TString sha]
  expectParcelRejection "duplicate original execution graphs" decodeExecutionSourceDescriptors parcel
  expectParcelRejection "duplicate original execution references" decodeExecutionSourceReferences (encode (TList [reference, reference]))
  putStrLn "execution source decoder: 21 admission cases passed"

executionSourceFileCustodyChecks :: ExecutionSourceGraph -> IO ()
executionSourceFileCustodyChecks graph = bracket newRoot removePathForcibly $ \root -> do
  let owner = root </> "owned-parent"
      request = root </> "current-request"
      manifest = request </> "scope.cbor"
      graphPath = owner </> "execution.cbor"
      sha = executionGraphSha256 graph
      descriptors = [(sha, graphPath)]
      readScope known = readExecutionSourceGraphs (RetainedScopeGraphFiles manifest) known descriptors
      reject label action = do
        result <- try action :: IO (Either IOException [ExecutionSourceGraph])
        case result of
          Left _ -> pure ()
          Right _ -> fail ("graph transport admitted " ++ label)
  createDirectoryIfMissing True owner
  createDirectoryIfMissing True request
  BS.writeFile graphPath (executionGraphBytes graph)
  forM_ [[], [graph]] $ \known -> do
    actual <- readScope known
    unless (actual == [graph]) (fail "retained parent graph transport changed the original payload")
  forM_ [[],[graph]] $ \known -> do
    actual <- readExecutionSourceGraphs (CandidateGraphFiles manifest) known descriptors
    unless (actual == [graph]) (fail "candidate transport lost acquired parent graph custody")
  reject "another advertised digest"
    (readExecutionSourceGraphs (RetainedScopeGraphFiles manifest) [] [(replicate 64 '0', graphPath)])
  reject "duplicate graph descriptors"
    (readExecutionSourceGraphs (RetainedScopeGraphFiles manifest) [] (descriptors ++ descriptors))
  reject "relative retained graph path"
    (readExecutionSourceGraphs (RetainedScopeGraphFiles manifest) [] [(sha, "execution.cbor")])
  BS.writeFile graphPath (BS.reverse (executionGraphBytes graph))
  reject "corrupted new graph" (readScope [])
  reject "corrupted previously captured graph" (readScope [graph])
  reject "corrupted acquired candidate graph"
    (readExecutionSourceGraphs (CandidateGraphFiles manifest) [graph] descriptors)
  removePathForcibly owner
  reject "expired parent graph custody" (readScope [])
  reject "expired acquired candidate graph custody"
    (readExecutionSourceGraphs (CandidateGraphFiles manifest) [] descriptors)
  where
    newRoot = do
      temporary <- getTemporaryDirectory
      (path, handle) <- openBinaryTempFile temporary "tidepool-graph-custody"
      hClose handle
      removeFile path
      createDirectoryIfMissing True path
      pure path

-- The graph and original identity come from the owning recipe issuer above.
-- Mutations are refusals: unchanged interface bytes cannot authorize another
-- native product/version or select a different graph by last-write order.
inheritedIdentityChecks :: ExecutionSourceGraph -> IO ()
inheritedIdentityChecks graph = do
  original <- case [executionOwnerIdentity owner | owner <- executionGraphOwners graph
      , executionModule (executionOwnerIdentity owner) == "Support"] of
    [identity] -> pure identity
    _ -> fail "issued inherited fixture lacks one support owner"
  let key = executionIdentityKey original
      reference = ExecutionSourceRef original (executionGraphSha256 graph)
      owner = ExecutionSourceOwner original False (Just (executionGraphSha256 graph))
      changedNative = original {executionNativeSha256 = shaHex "different native product"}
      changedVersion = original {executionVersion = shaHex "different module version"}
      changedInterface = original {executionIfaceSha256 = shaHex "different interface"}
      otherGraph = reference {executionRefGraph = shaHex "different original recipe"}
      positive =
        [ ("matching original", [reference], [original], [owner])
        , ("missing optional recipe", [], [original], [owner {executionOwnerOriginalGraph = Nothing}])
        , ("identical repeated input", [reference,reference], [original,original], [owner]) ]
      negative =
        [("changed retained " ++ label, [reference], [changed])
          | (label,changed) <- [("native",changedNative),("version",changedVersion),("interface",changedInterface)]]
        ++ [("conflicting original reference", refs, [original])
          | changed <- [changedNative,changedVersion]
          , refs <- [[reference,reference {executionRefIdentity=changed}],
                     [reference {executionRefIdentity=changed},reference]]]
        ++ [("conflicting graph reference", refs, [original])
          | refs <- [[reference,otherGraph],[otherGraph,reference]]]
        ++ [("conflicting retained identities", [], originals)
          | originals <- [[original,changedNative],[changedNative,original]]]
  forM_ positive $ \(label,references,originals,expected) ->
    unless (executionSourceInheritedOwners references originals == Right expected)
      (fail ("inherited reference lost " ++ label))
  forM_ negative $ \(label,references,originals) ->
    unless (executionSourceInheritedOwners references originals == Left (ExecutionSourceConflicting key))
      (fail ("inherited reference admitted " ++ label))
  putStrLn ("execution source inherited identity: " ++ show (length positive + length negative) ++ " cases passed")

-- Independently issued graphs can retain the same child as fresh in one
-- transaction and inherited in another. Sharing still authenticates both
-- original dependency recipes, including their negative search witnesses.
contextualDependencyChecks :: IO ()
contextualDependencyChecks = do
  let sha = replicate 64 'a'
      original name = ExecutionSourceIdentity "main" name sha sha sha
      parent = original "Parent"
      child = original "Child"
      wrapper = original "Wrapper"
      source = "module Expr where\nanswer = 42\n"
      path name = "/decoder-fixture/" ++ name ++ ".hs"
      node name imports = DependencyModule "main" name False (path name)
        [DependencyImport DependencyUnqualified imported False (Just (path imported))
        | imported <- imports] ProductReady
      evidence = DependencyEvidence True True
        (DependencySource "@generated-source" (shaHex (TE.encodeUtf8 (T.pack source)))
          : [DependencySource (path name) sha | name <- ["Child","Parent","Wrapper"]])
        [DependencyResolution DependencyUnqualified "Prelude" False Nothing [path "Absent"]]
        [] [node "Child" [],node "Parent" ["Child"],node "Wrapper" ["Parent"]]
      recipe = ExecutionSourceRecipe sha Nothing [] (path "Expr",source) evidence
        [ExecutionSourceOwner child True Nothing,ExecutionSourceOwner parent True Nothing,
         ExecutionSourceOwner wrapper True Nothing] [] []
      issue value = either (fail . show) (maybe (fail "context recipe was withheld") pure)
        (issueExecutionSourceRecipe value)
      reference identity graph = ExecutionSourceRef identity (executionGraphSha256 graph)
  fresh <- issue recipe
  retained <- issue recipe {recipeOwners=
    [ExecutionSourceOwner child False (Just (executionGraphSha256 fresh)),
     ExecutionSourceOwner parent True Nothing,ExecutionSourceOwner wrapper True Nothing]}
  let graphs = [fresh,retained]
      parentRef = reference parent fresh
      childRef = reference child fresh
      wrapperRef = reference wrapper retained
      refs = [parentRef,wrapperRef]
      expectShared result = do
        nodes <- either (fail . show) pure result
        case [value | value <- nodes,executionNodeIdentity value == parent] of
          [value] | executionNodeOriginalGraphs value == Set.fromList
              (map executionGraphSha256 graphs) -> pure ()
          _ -> fail "fresh/inherited dependency contexts lost their shared exact original"
  expectShared (executionSourceOriginalClosure graphs refs)
  expectShared (executionSourceOriginalClosure graphs (reverse refs))
  expectShared (executionSourceClosure graphs [parentRef,childRef,wrapperRef]
    [parent,child,wrapper] [("main","Parent"),("main","Wrapper")])
  changed <- issue recipe {recipeEvidence=evidence {dependencySources=
    [if dependencySourcePath row == path "Child"
      then row {dependencySourceSha256=replicate 64 'b'} else row
    | row <- dependencySources evidence]}}
  invalid <- issue recipe {recipeOwners=
    [ExecutionSourceOwner child False (Just (executionGraphSha256 changed)),
     ExecutionSourceOwner parent True Nothing,ExecutionSourceOwner wrapper True Nothing]}
  unless (case executionSourceOriginalClosure [fresh,changed,invalid]
      [parentRef,reference wrapper invalid] of
        Left (ExecutionSourceConflicting ("main","Child")) -> True; _ -> False)
    (fail "context sharing discarded a changed original child source recipe")
  unless (case executionSourceOriginalClosure [retained] [wrapperRef] of
      Left (ExecutionSourceMissing ("main","Child")) -> True; _ -> False)
    (fail "context sharing discarded a missing promised original child graph")
  let changedChild = child {executionVersion=replicate 64 'b',executionNativeSha256=replicate 64 'b'}
  changedNative <- issue recipe {recipeOwners=
    [ExecutionSourceOwner changedChild True Nothing,ExecutionSourceOwner parent True Nothing,
     ExecutionSourceOwner wrapper True Nothing]}
  conflicting <- issue recipe {recipeOwners=
    [ExecutionSourceOwner changedChild False (Just (executionGraphSha256 changedNative)),
     ExecutionSourceOwner parent True Nothing,ExecutionSourceOwner wrapper True Nothing]}
  forM_ [[parentRef,reference wrapper conflicting],[reference wrapper conflicting,parentRef]] $ \roots ->
    unless (case executionSourceOriginalClosure [fresh,changedNative,conflicting] roots of
        Left (ExecutionSourceConflicting _) -> True; _ -> False)
      (fail "context sharing accepted the same interface with a different original child native identity")
  putStrLn "execution source dependency contexts: 7 cases passed"

-- Candidate-path fanout is independent of the number of resolution rows.
-- These compact packets stay below the graph byte bound throughout.
executionSourceResolutionBudgetChecks :: IO ()
executionSourceResolutionBudgetChecks = do
  bytes <- fixtureBytes
  graph <- either fail pure (decode bytes)
  term <- case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
    Right (remaining,value) | BL.null remaining -> pure value
    _ -> fail "issued resolution fixture has malformed CBOR"
  let path = TString "/decoder-fixture/Absent.hs"
      resolution index candidates = TList
        [TString (T.pack (renderDependencyQualifier DependencyUnqualified)),
         TString (T.pack ("Missing" ++ show index)),TBool False,TNull,TList candidates]
      rows counts = TList [resolution index (replicate count path)
        | (index,count) <- zip [0 :: Int ..] counts]
      packet inventory = encode (at 7 (at 3 (const inventory)) term)
      accepted label inventory = either (fail . ((label ++ ": ") ++))
        (\decoded -> evaluate (forceGraph decoded) >> pure ()) (decode (packet inventory))
      refused label inventory = case decode (packet inventory) of
        Left reason | "original execution inventory exceeds bound" `isInfixOf` reason -> pure ()
        _ -> fail ("resolution candidate path budget admitted " ++ label)
      atLimit = rows (replicate 16 4096)
      overLimit = rows (replicate 16 4096 ++ [1])
      malformedAfterLimit = case atLimit of
        TList firstRows -> TList (firstRows ++ [resolution (16 :: Int) [TBool False]])
        _ -> error "resolution fixture inventory is not a list"
      recipe evidence = ExecutionSourceRecipe (executionGraphProducer graph)
        (executionGraphSemantic graph) (executionGraphIncludes graph)
        (executionGeneratedOrigin graph) evidence (executionGraphOwners graph)
        (executionGraphExactImports graph) (executionGraphPackages graph)
  accepted "65536 aggregate candidate paths" atLimit
  refused "65537 individually valid aggregate candidate paths" overLimit
  refused "candidate budget before candidate element decoding" malformedAfterLimit
  refused "4097 candidates in one row" (rows [4097])
  accepted "65536 empty resolution rows" (rows (replicate 65536 0))
  refused "65537 resolution rows" (rows (replicate 65537 0))
  boundedGraph <- either fail pure (decode (packet atLimit))
  case issueExecutionSourceRecipe (recipe (executionGraphEvidence boundedGraph)) of
    Right (Just _) -> pure ()
    _ -> fail "local recipe withheld the aggregate candidate path boundary"
  -- Build the final row directly: it is refused before encoding the graph.
  let excessive = (executionGraphEvidence boundedGraph) {dependencyResolutions=
        dependencyResolutions (executionGraphEvidence boundedGraph) ++
        [DependencyResolution DependencyUnqualified "Missing16" False Nothing ["/decoder-fixture/Absent.hs"]]}
  case issueExecutionSourceRecipe (recipe excessive) of
    Right Nothing -> pure ()
    _ -> fail "local recipe encoded excessive aggregate candidate paths"
  putStrLn "execution source resolution budget: eight aggregate, row, preallocation and issuer cases passed"

chars :: String -> Int
chars = foldl' (\total character -> total + ord character) 0

optional :: (value -> Int) -> Maybe value -> Int
optional force = maybe 0 force

qualifier :: DependencyQualifier -> Int
qualifier DependencyUnqualified = 1
qualifier (DependencyThisUnit value) = 2 + chars value
qualifier (DependencyOtherUnit value) = 3 + chars value

boolean :: Bool -> Int
boolean value = if value then 1 else 0

forceIdentity :: ExecutionSourceIdentity -> Int
forceIdentity original = sum (map chars [executionUnit original, executionModule original,
  executionVersion original, executionIfaceSha256 original, executionNativeSha256 original])

forceModule :: DependencyModule -> Int
forceModule node = chars (dependencyModuleUnit node) + chars (dependencyModuleName node)
  + boolean (dependencyModuleBoot node) + chars (dependencyModuleSource node)
  + (dependencyModuleProduct node `seq` 1)
  + sum [qualifier (dependencyImportQualifier edge) + chars (dependencyImportName edge)
      + boolean (dependencyImportBoot edge) + optional chars (dependencyImportSelected edge)
    | edge <- dependencyModuleImports node]

-- Force every typed field and every String character. Strict ByteString input
-- is already read and authenticated by SHA256 on each owning decode call.
forceGraph :: ExecutionSourceGraph -> Int
forceGraph graph = chars (executionGraphSha256 graph) + BS.length (executionGraphBytes graph)
  + chars (executionGraphProducer graph) + optional chars (executionGraphSemantic graph)
  + sum (map chars (executionGraphIncludes graph))
  + chars (fst (executionGeneratedOrigin graph)) + chars (snd (executionGeneratedOrigin graph))
  + boolean (dependencyCacheSafe evidence) + boolean (dependencySelectionComplete evidence)
  + sum [chars (dependencySourcePath row) + chars (dependencySourceSha256 row) | row <- dependencySources evidence]
  + sum [qualifier (dependencyResolutionQualifier row) + chars (dependencyResolutionModule row)
      + boolean (dependencyResolutionBoot row) + optional chars (dependencyResolutionSelected row)
      + sum (map chars (dependencyResolutionCandidates row)) | row <- dependencyResolutions evidence]
  + sum (map chars (dependencyPackages evidence)) + sum (map forceModule (dependencyModules evidence))
  + sum [forceIdentity (executionOwnerIdentity owner') + boolean (executionOwnerFresh owner')
      + optional chars (executionOwnerOriginalGraph owner') | owner' <- executionGraphOwners graph]
  + sum [chars unit' + chars name + sum [chars unit'' + chars name' | (unit'', name') <- imports]
      | ((unit', name), imports) <- executionGraphExactImports graph]
  + sum [chars unit' + chars name + chars path + chars sha | (unit', name, path, sha) <- executionGraphPackages graph]
  where evidence = executionGraphEvidence graph

forceNode :: ExecutionSourceNode -> Int
forceNode node = forceIdentity (executionNodeIdentity node) + forceModule (executionNodeModule node)
  + chars (executionNodeSourceSha256 node)
  + sum [chars unit' + chars name | (unit', name) <- executionNodeRequirements node]

data Demand = FullGraph | OriginalNodeAdmission ExecutionSourceIdentity | MaterializedNode ExecutionSourceIdentity

-- Compare these complete decoded-value snapshots byte for byte between
-- baseline and candidate. Graph Eq compares the authenticated input bytes,
-- while the benchmark's numeric checksum is only a forcing witness.
executionSourceDecodeSnapshots :: FilePath -> [FilePath] -> IO ()
executionSourceDecodeSnapshots output files = do
  createDirectoryIfMissing True output
  forM_ files $ \path -> do
    bytes <- BS.readFile path
    let sha = shaHex bytes
    graph <- either fail pure (decodeExecutionSourceGraph sha bytes)
    BS.writeFile (output </> sha ++ ".decoded.cbor") (encode (snapshot graph))
  where
    string = TString . T.pack
    list item = TList . map item
    maybe' item = maybe TNull item
    key (unit, name) = TList [string unit, string name]
    identity' original = list string [executionUnit original, executionModule original,
      executionVersion original, executionIfaceSha256 original, executionNativeSha256 original]
    module' node = TList [string (dependencyModuleUnit node), string (dependencyModuleName node),
      TBool (dependencyModuleBoot node), string (dependencyModuleSource node),
      list (\edge -> TList [string (renderDependencyQualifier (dependencyImportQualifier edge)),
        string (dependencyImportName edge), TBool (dependencyImportBoot edge),
        maybe' string (dependencyImportSelected edge)]) (dependencyModuleImports node),
      string (show (dependencyModuleProduct node))]
    snapshot graph = let evidence = executionGraphEvidence graph in TList
      [ string (executionGraphSha256 graph), string (executionGraphProducer graph),
        maybe' string (executionGraphSemantic graph), list string (executionGraphIncludes graph),
        TList [string (fst (executionGeneratedOrigin graph)), string (snd (executionGeneratedOrigin graph))],
        TList [TBool (dependencyCacheSafe evidence), TBool (dependencySelectionComplete evidence),
          list (\row -> TList [string (dependencySourcePath row), string (dependencySourceSha256 row)]) (dependencySources evidence),
          list (\row -> TList [string (renderDependencyQualifier (dependencyResolutionQualifier row)),
            string (dependencyResolutionModule row), TBool (dependencyResolutionBoot row),
            maybe' string (dependencyResolutionSelected row), list string (dependencyResolutionCandidates row)]) (dependencyResolutions evidence),
          list string (dependencyPackages evidence), list module' (dependencyModules evidence)],
        list (\owner' -> TList [identity' (executionOwnerIdentity owner'), TBool (executionOwnerFresh owner'),
          maybe' string (executionOwnerOriginalGraph owner')]) (executionGraphOwners graph),
        list (\(owner', imports) -> TList [key owner', list key imports]) (executionGraphExactImports graph),
        list (\(unit, name, path, sha) -> list string [unit, name, path, sha]) (executionGraphPackages graph)]

{-# NOINLINE decodedDemand #-}
decodedDemand :: Demand -> String -> BS.ByteString -> Either String Int
decodedDemand demand sha bytes = do
  graph <- decodeExecutionSourceGraph sha bytes
  let selected original force = case executionSourceOriginalNode [graph] original sha of
        Left reason -> Left (show reason)
        Right node -> pure (force node)
  case demand of
    FullGraph -> pure (forceGraph graph)
    OriginalNodeAdmission original -> selected original (const 1)
    MaterializedNode original -> selected original forceNode

-- Runs unchanged on baseline and candidate libraries. Full demand is distinct
-- from ExactScope's original-node validation, which discards the returned node,
-- and from materializing its source/import fields for downstream loading.
-- Neither node workload consumes unrelated resolution-candidate strings.
executionSourceDecodeBenchmark :: String -> [FilePath] -> IO ()
executionSourceDecodeBenchmark size files = do
  iterations <- case readMaybe size :: Maybe Int of
    Just value | value > 0 && not (null files) -> pure value
    _ -> fail "decoder benchmark needs positive iterations and captured graph files"
  statistics <- getRTSStatsEnabled
  unless statistics (fail "decoder benchmark requires +RTS -T")
  forM_ files $ \path -> do
    bytes <- BS.readFile path
    sha <- evaluate (shaHex bytes)
    graph <- either fail pure (decodeExecutionSourceGraph sha bytes)
    _ <- evaluate (forceGraph graph)
    let available = [executionOwnerIdentity owner' | owner' <- executionGraphOwners graph,
          Right _ <- [executionSourceOriginalNode [graph] (executionOwnerIdentity owner') sha]]
    chosen <- case available of
      original : _ -> pure original
      [] -> fail "captured graph has no selectable original source node"
    input <- newIORef (sha, bytes)
    forM_ [1 :: Int .. 4] $ \repetition -> do
      let modes :: [(String, Demand)]
          modes = [("fully-forced", FullGraph),
            ("original-node-admission", OriginalNodeAdmission chosen),
            ("materialized-node", MaterializedNode chosen)]
      forM_ (if odd repetition then modes else reverse modes) $ \(mode, demand) -> do
        expected <- either fail evaluate (decodedDemand demand sha bytes)
        performGC
        before <- getRTSStats
        startCpu <- getCPUTime
        startWall <- getMonotonicTimeNSec
        forM_ [1..iterations] $ \_ -> do
          (currentSha, currentBytes) <- readIORef input
          actual <- either fail evaluate (decodedDemand demand currentSha currentBytes)
          when (actual /= expected) (fail "decoder benchmark demand changed")
        stopWall <- getMonotonicTimeNSec
        stopCpu <- getCPUTime
        performGC
        after <- getRTSStats
        putStrLn ("{\"input_sha256\":" ++ show sha ++ ",\"encoded_bytes\":" ++ show (BS.length bytes)
          ++ ",\"mode\":" ++ show mode ++ ",\"checksum\":" ++ show expected
          ++ ",\"iterations\":" ++ show iterations ++ ",\"repetition\":" ++ show repetition
          ++ ",\"wall_ns\":" ++ show (stopWall-startWall) ++ ",\"cpu_ps\":" ++ show (stopCpu-startCpu)
          ++ ",\"allocated_bytes\":" ++ show (allocated_bytes after-allocated_bytes before) ++ "}")
