module TypeEvidenceChecks
  ( runTypeEvidenceChecks, runTypeIdentityParityChecks ) where

import Control.Monad (unless, forM_)
import Control.Monad.State.Strict (runStateT, lift)
import Data.IntMap.Strict qualified as IntMap
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC.Builtin.Types (boolTy, tupleTyCon)
import GHC.Types.Basic (Boxity(Boxed))
import GHC.Core.Coercion (mkNomReflCo)
import GHC.Core.Type (typeKind, mkNumLitTy)
import GHC.Core.TyCo.Rep (Type(..))
import Tidepool.CanonicalTypeShape (captureClosedTypeShape, TypeShapeError(..))
import GHC.Core.DataCon (dataConName)
import GHC.Core.TyCon (tyConDataCons, tyConName)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.TypeEnv (typeEnvTyCons)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), lookupHpt)
import GHC.Unit.Module (moduleName, moduleNameString)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModIface (mi_module)
import GHC.Driver.Env (hsc_HPT)
import System.Directory (createDirectoryIfMissing)
import System.FilePath ((</>))
import Tidepool.CompilerProducts (prepareCompilerProjectionContext)
import Tidepool.ConstructorIdentity (captureIdentityResolver, resolveTypeIdentity)
import Tidepool.EffectSchema (ysSite)
import Tidepool.ExactHydration (freshExactState, hydrateOriginalInterfaces)
import Tidepool.ExecutionProjection (ProjectionContext(..), projectPreparedTarget)
import Tidepool.ExecutionSchema
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), PreparedPipelineResult, pprPipelineResult, pprModules, pprFinalizedModules
  , prHscEnv, finalizedHomeModInfo, runPipelineSelected )
import Tidepool.PreparedSites (PreparedSite(..), requestReplyIndex)
import Tidepool.PreparedStg (pmModule, pmIdentityResolver, pmPreparedSites, pmTypeGraph)
import Tidepool.TypePolicy qualified as Policy

-- Two genuine compiler results test alpha and same-layout field semantics. They remain different
-- source originals; graph equality never admits one as the other's certificate.
runTypeEvidenceChecks :: FilePath -> IO ()
runTypeEvidenceChecks directory = do
  (target, fixture) <- writeTypeEvidenceFixture directory
  original <- runPipelineSelected PreparedStg target [directory]
  assert (length [() | line <- lines fixture, line == "  Echo :: a -> Console a"] == 1)
    "alpha control must change exactly one actual constructor declaration"
  assert (length [() | line <- lines fixture, line == "data SameLayout = SameLayout Int"] == 1)
    "formal declaration conflict control must change exactly one actual field"
  writeFile target (unlines [case line of
    "  Echo :: a -> Console a" -> "  Echo :: reply -> Console reply"
    "data SameLayout = SameLayout Int" -> "data SameLayout = SameLayout Bool"
    _ -> line | line <- lines fixture])
  alpha <- runPipelineSelected PreparedStg target [directory]
  writeFile target fixture
  context <- contextFor original
  alphaContext <- contextFor alpha
  let project ctx result entry auxiliary = projectPreparedTarget
        (ctx { projectionEntry = (projectionEntry ctx) { symbolOccurrence = entry }
             , projectionAuxiliaryRoots = [(projectionEntry ctx) { symbolOccurrence = name }
                 | name <- auxiliary] }) (pprModules result)
      program entry = requireRight (project context original entry [])
      answer entry = do
        wire <- program entry
        case programSites wire of
          [site] -> pure (wire, rootBody (programTypes wire) (siteWire site))
          sites -> fail (Text.unpack entry ++ ": expected one site, got " ++ show sites)
      requireHead entry name form = do
        (wire, expression) <- answer entry
        let (identity, actual, _) = nominal (programTypes wire) expression
        assert (symbolOccurrence (typeSymbol identity) == name && actual == form)
          (Text.unpack entry ++ ": wrong declaration " ++ show (identity, actual))
        pure wire
  bool <- requireHead "boolAnswer" "Bool" DataDeclaration
  assert (length (templates bool "Bool") == 2) "Bool lost a constructor template"
  nested <- requireHead "nestedIdentity" "Identity" (NewtypeDeclaration 1)
  assert (any (\node -> case node of TypeBound 0 -> True; _ -> False)
    (IntMap.elems (typeGraphNodes (programTypes nested)))) "Identity lost its formal parameter"
  _ <- requireHead "higherKinded" "Wrap" (NewtypeDeclaration 1)
  loop <- requireHead "recursiveNewtype" "Loop" (NewtypeDeclaration 0)
  assert (IntMap.size (typeGraphNodes (programTypes loop)) < 256)
    "recursive newtype was expanded instead of linked"
  _ <- requireHead "impossibleGadt" "Choice"
    (OpaqueDeclaration NominalConstructor "existential or constrained constructor")
  _ <- requireHead "packedAnswer" "Packed" (OpaqueDeclaration NominalConstructor "layout")
  chain <- requireHead "recursiveData" "Chain" DataDeclaration
  assert (map (length . snd) (templates chain "Chain") == [0, 1])
    "recursive data lost its original field template"
  nest <- requireHead "expandingData" "Nest" DataDeclaration
  assert (IntMap.size (typeGraphNodes (programTypes nest)) < 256
      && map (length . snd) (templates nest "Nest") == [1])
    "nonregular recursion did not remain a finite declaration template"
  pair <- requireHead "expandingPair"
    (Text.pack (occNameString (nameOccName (tyConName (tupleTyCon Boxed 2))))) DataDeclaration
  faster <- requireHead "fasterExpanding" "Nest2" DataDeclaration
  assert (IntMap.size (typeGraphNodes (programTypes pair)) < 256
      && IntMap.size (typeGraphNodes (programTypes faster)) < 256
      && length (templates pair "Nest") == 1 && length (templates faster "Nest2") == 1)
    "multiple applications or faster-growing recursive syntax expanded field instances"
  forM_ [("phantomInt", "Phantom", ["Int"]), ("phantomBool", "Phantom", ["Bool"]),
         ("eitherIntBool", "Either", ["Int", "Bool"]), ("eitherBoolInt", "Either", ["Bool", "Int"])] $
    \(entry, name, expected) -> do
      (wire, expression) <- answer entry
      let (identity, _, arguments) = nominal (programTypes wire) expression
      assert (symbolOccurrence (typeSymbol identity) == name
        && map (headName (programTypes wire)) arguments == expected)
        "nominal type arguments lost their order or phantom identity"
  forM_ [("textAnswer", "Text", TextDeclaration, ["Text"]),
         ("integerAnswer", "Integer", IntegerDeclaration, ["IS", "IP", "IN"]),
         ("naturalAnswer", "Natural", NaturalDeclaration, ["NS", "NB"])] $
    \(entry, name, form, constructors) -> do
      wire <- requireHead entry name form
      assert (all (`elem` map (symbolOccurrence . constructorSymbol . constructorIdentity) (programConstructors wire)) constructors)
        "special building mode lost its authenticated physical constructors"
  forM_ [("printRequest", "Print"), ("customSend", "Print")] $ \(entry, occurrence) -> do
    wire <- program entry
    assert (headName (programTypes wire) (replyBody wire "TypeEvidence" occurrence) `elem` ["Unit", "()"])
      "intrinsic unsited reply lost its unit type"
  fetch <- program "fetchRequest"
  let (_, fetchForm, fetchArgs) = nominal (programTypes fetch) (replyBody fetch "TypeEvidence" "Fetch")
  assert (fetchForm == DataDeclaration && map (headName (programTypes fetch)) fetchArgs == ["Bool", "Text"])
    "Fetch lost Either Bool Text"
  echo <- program "echoRequest"
  alphaEcho <- requireRight (project alphaContext alpha "echoRequest" [])
  let echoRoot wire = replyRoot wire "TypeEvidence" "Echo"
      rendering wire = case nodeAt (programTypes wire) (echoRoot wire) of
        TypeRoot ConstructorSchemeRoot _ text -> text
        other -> error (show other)
  assert (rendering echo /= rendering alphaEcho) "alpha control did not change the actual diagnostic"
  assert (map (nodeAt (programTypes echo)) [rootBody (programTypes echo) (echoRoot echo)] == [TypeBound 0])
    "open reply lost its scoped bound variable"
  assert (namedConstructors echo "Echo" == namedConstructors alphaEcho "Echo")
    "alpha-renaming changed the physical constructor declaration"
  originalTemplate <- program "templateReply"
  changedTemplate <- requireRight (project alphaContext alpha "templateReply" [])
  assert (namedConstructors originalTemplate "SameLayout" == namedConstructors changedTemplate "SameLayout")
    "formal declaration conflict control changed physical layout instead of only field type"
  function <- program "functionRequest"
  assert (case nodeAt (programTypes function) (replyBody function "TypeEvidence" "FunctionReply") of
    TypeFunction{} -> True; _ -> False) "function reply lost exact syntax"
  forM_ [("partialReply", "PartialReply", "Maybe", [0, 1]),
         ("progressRequest", "ObserveProgress", "Progress", [0, 1, 0])] $
    \(entry, occurrence, name, arities) -> do
      wire <- program entry
      let (identity, form, arguments) = nominal (programTypes wire) (replyBody wire "TypeEvidence" occurrence)
      assert (symbolOccurrence (typeSymbol identity) == name && form == DataDeclaration
        && map (length . snd) (templates wire name) == arities
        && map (nodeAt (programTypes wire)) arguments == [TypeBound 0])
        "partial algebraic reply lost fieldless branches or its unresolved parameter"
  carrier <- program "genuineCarrier"
  assert (replyFor carrier "TypeEvidence" "GenuineCarrier" == [ReplyAtSite])
    "genuine request-site carrier lost AtSite selection"
  provenLeading <- program "provenLeading"
  assert (replyFor provenLeading "TypeEvidence" "ProvenLeading" == [ReplyAtSite])
    "erased GADT equality shifted the leading original site"
  forM_ [("provenSubmit", "ProvenSubmit", 1, 2, Nothing),
         ("provenPublication", "ProvenPublication", 1, 2, Just 1),
         ("provenProgressPublication", "ProvenProgressPublication", 1, 2, Just 2),
         ("provenExit", "ProvenExit", 1, 0, Just 0)] $ \(entry, occurrence, field, payload, capture) -> do
    wire <- program entry
    case replyFor wire "TypeEvidence" occurrence of
      [StaticReplyWithSite root actualField actualPayload actualCapture] -> do
        assert (actualField == field && actualPayload == payload && actualCapture == capture)
          "erased GADT evidence shifted original site/payload/capture positions"
        assert (headName (programTypes wire) (rootBody (programTypes wire) root) `elem` ["Unit", "()"])
          "original site changed the closed unit reply representation"
      replies -> fail ("GADT original site lost exact carrier evidence: " ++ show (occurrence, replies))
  forM_ [("dictionaryNonLeading", "DictionaryNonLeading"), ("splitCarrier", "SplitCarrier")] $
    \(entry, occurrence) -> do
      wire <- program entry
      assert (case replyFor wire "TypeEvidence" occurrence of [StaticReply _] -> True; _ -> False)
        "retained dictionary or split payload authorized a guessed original site position"
  forM_ [("mismatchedCarrier", "MismatchedCarrier"), ("strictCarrier", "StrictCarrier"),
         ("dictionaryCarrier", "DictionaryCarrier"), ("integerPayload", "IntegerPayload")] $
    \(entry, occurrence) -> do
      wire <- program entry
      assert (headName (programTypes wire) (replyBody wire "TypeEvidence" occurrence) == "Int")
        "carrier layout mismatch was admitted as AtSite"
  forM_ ["polyChoice", "polyChoiceNested"] $ \entry -> do
    wire <- program entry
    assert (headName (programTypes wire) (replyBody wire "TypeEvidence" ":|") == "Either")
      "ordinary saturated alternatives lost their schematic reply"
  profile <- program "profileWitness"
  assert (null (programConstructorReplies profile)) "effect-list witness acquired a reply root"
  forM_ [("agentAttachRequest", "AgentAttachWith"), ("agentToolsInstallRequest", "AgentToolsInstallWith")] $
    \(entry, occurrence) -> do
      wire <- program entry
      assert (headName (programTypes wire) (replyBody wire "Tidepool.Effects.Core" occurrence) `elem` ["Unit", "()"])
        "private protocol request lost unit evidence"
  empty <- program "unrelated"
  assert (null (programSites empty) && IntMap.null (typeGraphNodes (programTypes empty)))
    "unselected typed sites leaked into the artifact"
  auxiliary <- requireRight (project context original "unrelated" ["auxiliaryRootDecode"])
  assert (all (`elem` map (symbolOccurrence . constructorSymbol . constructorIdentity) (programConstructors auxiliary)) ["Left", "Right"])
    "auxiliary root lost its own result constructors"
  verifyOriginalScopes original alpha
  let cast = CastTy boolTy (mkNomReflCo (typeKind boolTy))
      coercion = CoercionTy (mkNomReflCo boolTy)
      failure ty = case runStateT (Policy.internType ty) Policy.emptyTypeGraphBuilder of
        Left category -> Just category
        Right _ -> Nothing
  assert (failure cast == Just Policy.TypeGraphCast && failure coercion == Just Policy.TypeGraphCoercion)
    "unsupported cast/coercion syntax acquired graph evidence"
  assert (case (captureClosedTypeShape cast, captureClosedTypeShape coercion) of
    (Left TypeShapeCast, Left TypeShapeCoercion) -> True; _ -> False)
    "closed activation changed its cast/coercion refusal classes"
  assert (case runStateT (mapM (Policy.internType . mkNumLitTy) [0 .. 32767]) Policy.emptyTypeGraphBuilder of
    Left Policy.TypeGraphNodeLimit -> True; _ -> False)
    "one shared aggregate node budget was reset across independent roots"


-- Each call runs a new compiler capture; source text equality is not used to
-- reconstruct a type version or stand in for the selected interface resolver.
runTypeIdentityParityChecks :: FilePath -> IO ()
runTypeIdentityParityChecks directory = do
  (target, fixture) <- writeTypeEvidenceFixture directory
  original <- runPipelineSelected PreparedStg target [directory]
  repeated <- runPipelineSelected PreparedStg target [directory]
  let edited = unlines [if line == "payloadOffset = 7" then "payloadOffset = 31" else line
        | line <- lines fixture]
  assert (length [() | line <- lines fixture, line == "payloadOffset = 7"] == 1)
    "body control must change exactly one authored offset"
  writeFile target edited
  changedBody <- runPipelineSelected PreparedStg target [directory]
  wires <- mapM projectNestedSite [original, repeated, changedBody]
  forM_ (zip [original, repeated, changedBody] wires) $ \(result, wire) -> do
    verifyHydratedSiteIdentities result wire
  case wires of
    first : rest -> forM_ rest $ \wire -> do
      assert (siteId (onlySite first) == siteId (onlySite wire))
        "identical capture or body-only edit changed the issued site identity"
      assert (siteIdentities first == siteIdentities wire)
        ("identical capture or body-only edit changed reachable nominal evidence: "
          ++ show (siteIdentities first, siteIdentities wire))
    _ -> fail "identity parity control did not capture its three compiler results"

projectNestedSite :: PreparedPipelineResult -> IO WireProgram
projectNestedSite result = do
  context <- contextFor result
  requireRight (projectPreparedTarget
    (context { projectionEntry = (projectionEntry context) { symbolOccurrence = "nestedVersion" } })
    (pprModules result))

onlySite :: WireProgram -> SiteRow
onlySite wire = case programSites wire of
  [site] -> site
  sites -> error ("nested identity fixture expected one selected site: " ++ show sites)

siteIdentities :: WireProgram -> Map.Map SymbolIdentity TypeIdentity
siteIdentities wire = Map.fromList
  [(typeSymbol identity, identity) | identity <- reachableDeclarations (programTypes wire)
    (siteWire (onlySite wire) : siteInputs (onlySite wire))]

-- Follow issued roots and graph edges rather than a module-wide type inventory.
reachableDeclarations :: TypeGraphF constructor identity rep -> [TypeNodeId] -> [identity]
reachableDeclarations graph = go Set.empty
 where
  go _ [] = []
  go visited (node@(TypeNodeId raw) : pending)
    | node `Set.member` visited = go visited pending
    | otherwise = let next = map snd (IntMap.findWithDefault [] (fromIntegral raw) (typeGraphEdges graph))
                      rest = go (Set.insert node visited) (next ++ pending)
                  in case IntMap.lookup (fromIntegral raw) (typeGraphNodes graph) of
                    Just (TypeDeclaration identity _ _ _) -> identity : rest
                    Just _ -> rest
                    Nothing -> error "issued type graph contains a missing reachable node"

verifyHydratedSiteIdentities :: PreparedPipelineResult -> WireProgram -> IO ()
verifyHydratedSiteIdentities result wire = do
  prepared <- case [value | value <- pprModules result
    , moduleNameString (moduleName (pmModule value)) == "TypeEvidence"] of
      [value] -> pure value
      _ -> fail "identity parity lacks its original prepared module"
  site <- case [value | value <- pmPreparedSites prepared
    , ysSite (psSite value) == siteId (onlySite wire)] of
      [value] -> pure value
      _ -> fail "identity parity lacks the compiler-issued site graph roots"
  let infos = map finalizedHomeModInfo (Map.elems (pprFinalizedModules result))
      environment = prHscEnv (pprPipelineResult result)
      types = reachableDeclarations (pmTypeGraph prepared) (psWireNode site : psInputNodes site)
  isolated <- freshExactState environment
  hydrated <- hydrateOriginalInterfaces isolated (map hm_iface infos)
  hydratedResolver <- captureIdentityResolver hydrated Nothing
  source <- mapM (requireRight . resolveTypeIdentity (pmIdentityResolver prepared)) types
  restored <- mapM (requireRight . resolveTypeIdentity hydratedResolver) types
  let inventory identities = Map.fromList [(typeSymbol identity, identity) | identity <- identities]
  assert (not (null source) && inventory source == siteIdentities wire)
    "projected reachable nominal identities differ from their issuing prepared resolver"
  assert (inventory source == inventory restored)
    ("exact interface hydration changed reachable compiler-issued nominal identities: "
      ++ show (inventory source, inventory restored))

contextFor :: PreparedPipelineResult -> IO ProjectionContext
contextFor result = case [pmModule prepared | prepared <- pprModules result,
  moduleNameString (moduleName (pmModule prepared)) == "TypeEvidence"] of
    [owner] -> prepareCompilerProjectionContext result mempty owner "unrelated" [] Nothing
    _ -> fail "type evidence fixture lacks its unique original module"

writeTypeEvidenceFixture :: FilePath -> IO (FilePath, String)
writeTypeEvidenceFixture directory = do
  let target = directory </> "TypeEvidence.hs"
  createDirectoryIfMissing True (directory </> "Tidepool" </> "Effects")
  writeFile (directory </> "Tidepool" </> "Effects" </> "Core.hs") (unlines
    [ "{-# LANGUAGE ExplicitForAll, GADTs #-}"
    , "module Tidepool.Effects.Core where"
    , "data AgentSession a where"
    , "  AgentAttachWith :: Maybe String -> AgentSession ()"
    , "data AgentTools a where"
    , "  AgentToolsInstallWith :: AgentTools ()" ])
  createDirectoryIfMissing True (directory </> "Tidepool" </> "Internal")
  readFile "lib/Tidepool/Internal/RequestSite.hs" >>= writeFile
    (directory </> "Tidepool" </> "Internal" </> "RequestSite.hs")
  writeFile (directory </> "Tidepool" </> "Actor.hs") (unlines
    [ "{-# LANGUAGE DataKinds, ExplicitForAll #-}"
    , "module Tidepool.Actor where"
    , "import Tidepool.Internal.RequestSite (RequestSite)"
    , "{-# OPAQUE receive #-}"
    , "receive :: forall answer. String -> Maybe answer"
    , "receive _ = Nothing"
    , "{-# OPAQUE receiveSited #-}"
    , "receiveSited :: forall answer. RequestSite '[] answer -> String -> Maybe answer"
    , "receiveSited _ _ = Nothing" ])
  fixture <- readFile "test-prepared-stg/site-fixtures/TypeEvidence.hs"
  writeFile target fixture
  pure (target, fixture)

-- Source and genuine original-interface hydration share one builder. This
-- tests producer semantics, not Rust certificate admission; frozen M2 checks
-- the same-original BranchU custody and repeated installation path.
verifyOriginalScopes :: PreparedPipelineResult -> PreparedPipelineResult -> IO ()
verifyOriginalScopes original alpha = do
  let infos result = map finalizedHomeModInfo (Map.elems (pprFinalizedModules result))
      constructors values = concatMap (concatMap tyConDataCons . typeEnvTyCons . md_types . hm_details)
        [info | info <- values, moduleNameString (moduleName (mi_module (hm_iface info))) == "TypeEvidence"]
      originalConstructors = constructors (infos original)
      select name values = case [constructor | constructor <- values,
        occNameString (nameOccName (dataConName constructor)) == name] of
          [constructor] -> pure constructor
          _ -> fail ("missing or duplicated constructor " ++ name)
      issue constructor builder = maybe (Left Policy.TypeGraphOriginalDeclarationMismatch)
        (\reply -> runStateT (Policy.internConstructorType constructor reply) builder) (requestReplyIndex constructor)
      capture constructor builder = requireRight (issue constructor builder)
  first <- select "Echo" originalConstructors
  second <- select "Echo" (constructors (infos alpha))
  (firstRoot, initial) <- capture first Policy.emptyTypeGraphBuilder
  (secondRoot, shared) <- capture second initial
  assert (firstRoot == secondRoot) "alpha-equivalent original replies did not share one graph root"
  let names = ["FirstScope", "SecondScope", "RepeatedScope", "DistinctScope",
        "AlphaScope", "AlphaRenamed", "UnusedScope", "NoUnusedScope", "HigherKindScope"]
  scoped <- mapM (`select` originalConstructors) names
  (roots, scopedBuilder) <- requireRight (runStateT
    (mapM (\constructor -> maybe (lift (Left Policy.TypeGraphOriginalDeclarationMismatch))
      (Policy.internConstructorType constructor) (requestReplyIndex constructor)) scoped) shared)
  assert (length roots == 9 && roots !! 0 /= roots !! 1 && roots !! 2 /= roots !! 3
      && roots !! 4 == roots !! 5 && roots !! 6 /= roots !! 7 && roots !! 6 /= roots !! 8)
    "shared graph conflated alpha, binder position, repetition, unused binders or kinds"
  intConstructor <- select "OnlyInt" originalConstructors
  intReply <- maybe (fail "OnlyInt lacks its reply") pure (requestReplyIndex intConstructor)
  (closed, closedBuilder) <- requireRight (runStateT (Policy.internType intReply) scopedBuilder)
  (schematic, schemeBuilder) <- capture intConstructor closedBuilder
  assert (closed /= schematic) "closed activation and constructor-scheme domains were conflated"
  isolated <- freshExactState (prHscEnv (pprPipelineResult original))
  hydrated <- hydrateOriginalInterfaces isolated (map hm_iface (infos original))
  hydratedInfos <- mapM (\info -> maybe (fail "hydrated original interface missing") pure
    (lookupHpt (hsc_HPT hydrated) (moduleName (mi_module (hm_iface info))))) (infos original)
  let hydratedConstructors = constructors hydratedInfos
  (_, final) <- foldScopes capture select originalConstructors hydratedConstructors schemeBuilder
  originalTemplate <- select "TemplateReply" originalConstructors
  changedTemplate <- select "TemplateReply" (constructors (infos alpha))
  (_, templateBuilder) <- capture originalTemplate final
  assert (case issue changedTemplate templateBuilder of
    Left Policy.TypeGraphOriginalDeclarationMismatch -> True; _ -> False)
    "same nominal declaration silently accepted different formal field types with equal layout"
  graph <- requireRight (Policy.finishTypeGraph templateBuilder)
  putStrLn ("finite original reply graph: " ++ show (length originalConstructors)
    ++ " original constructors, " ++ show (IntMap.size (Policy.tgNodes graph))
    ++ " shared nodes; alpha/scope/domain/source-interface controls passed")
 where
  foldScopes capture select sources hydrated = go sources
   where
    go [] builder = pure ((), builder)
    go (constructor : rest) builder = case requestReplyIndex constructor of
      Nothing -> go rest builder
      Just _ -> do
        (sourceRoot, next) <- capture constructor builder
        other <- select (occNameString (nameOccName (dataConName constructor))) hydrated
        (hydratedRoot, completed) <- capture other next
        assert (sourceRoot == hydratedRoot) "original interface hydration changed the complete source scheme"
        go rest completed

assert :: Bool -> String -> IO ()
assert condition message = unless condition (fail message)
requireRight :: Show e => Either e a -> IO a
requireRight = either (fail . show) pure
nodeAt :: TypeGraph -> TypeNodeId -> TypeNode
nodeAt graph (TypeNodeId index) = maybe (error "missing finite graph node") id
  (IntMap.lookup (fromIntegral index) (typeGraphNodes graph))
outgoing :: TypeGraph -> TypeNodeId -> [(TypeEdgeRoleF RuntimeRep, TypeNodeId)]
outgoing graph (TypeNodeId index) = IntMap.findWithDefault [] (fromIntegral index) (typeGraphEdges graph)
edge :: TypeGraph -> TypeNodeId -> TypeEdgeRoleF RuntimeRep -> TypeNodeId
edge graph root role = case [target | (actual, target) <- outgoing graph root, actual == role] of
  [target] -> target
  _ -> error ("missing/duplicate finite graph edge " ++ show role)
rootBody :: TypeGraph -> TypeNodeId -> TypeNodeId
rootBody graph root = case nodeAt graph root of TypeRoot{} -> edge graph root TypeBody; _ -> error "reply is not a scoped root"
nominal :: TypeGraph -> TypeNodeId -> (TypeIdentity, DeclarationFormF RuntimeRep, [TypeNodeId])
nominal graph expression = case nodeAt graph expression of
  TypeNominalApplication -> case nodeAt graph (edge graph expression TypeHead) of
    TypeDeclaration identity _ form _ -> (identity, form, [target | (TypeArgument _, target) <- outgoing graph expression])
    _ -> error "nominal expression has no declaration"
  other -> error ("expected nominal expression, got " ++ show other)
headName :: TypeGraph -> TypeNodeId -> Text.Text
headName graph expression = let (identity, _, _) = nominal graph expression in symbolOccurrence (typeSymbol identity)
namedConstructors :: WireProgram -> Text.Text -> [ConstructorDecl]
namedConstructors wire name = filter ((== name) . symbolOccurrence . constructorSymbol . constructorIdentity) (programConstructors wire)
replyFor :: WireProgram -> Text.Text -> Text.Text -> [ConstructorReply]
replyFor wire owner name = [reply | (ConstructorId index, reply) <- programConstructorReplies wire,
  let identity = constructorSymbol (constructorIdentity (programConstructors wire !! fromIntegral index)),
  symbolModule identity == owner, symbolOccurrence identity == name]
replyRoot :: WireProgram -> Text.Text -> Text.Text -> TypeNodeId
replyRoot wire owner name = case replyFor wire owner name of [StaticReply root] -> root; other -> error (show other)
replyBody :: WireProgram -> Text.Text -> Text.Text -> TypeNodeId
replyBody wire owner name = rootBody (programTypes wire) (replyRoot wire owner name)
templates :: WireProgram -> Text.Text -> [(Word, [(TypeEdgeRoleF RuntimeRep, TypeNodeId)])]
templates wire name = [(fromIntegral tag, outgoing graph target)
  | (index, TypeDeclaration identity _ DataDeclaration _) <- IntMap.toAscList (typeGraphNodes graph),
    symbolOccurrence (typeSymbol identity) == name, (TypeConstructor tag, target) <- outgoing graph (TypeNodeId (fromIntegral index))]
 where graph = programTypes wire
