-- | Parsed-source lowering for compiler-owned typed effect segments.
-- Authored nested do expressions remain opaque to this rewrite.
module Tidepool.TypedSegment.Source
  ( rewriteParsedSegmentRoot, typedSegmentInputOccurrences
  , ParsedSegmentDemand, prepareSegmentDemand, segmentDemandMatchesSource
  , SegmentDemandScope, newSegmentDemandScope, updateSegmentDemandScope
  ) where

import Control.Exception (throwIO)
import Control.Monad (forM, foldM)
import qualified Data.ByteString as BS
import Data.Data (Data)
import Data.Generics (everything, mkQ)
import Data.List (intersect, nub)
import qualified Data.Map.Strict as Map
import qualified Data.Text as Text
import qualified Data.Text.Encoding as TextEncoding
import GHC (ParsedModule (..))
import GHC.Driver.Session (DynFlags, GeneralFlag(Opt_HelpfulErrors,Opt_DeferOutOfScopeVariables), gopt_set, gopt_unset, xopt)
import GHC.Driver.Env (HscEnv, hsc_NC, hscUpdateFlags)
import GHC.Iface.Env (allocateGlobalBinder)
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Rename.Module (findSplice, rnSrcDecls)
import GHC.Rename.Names (rnImports)
import GHC.Tc.Types (TcGblEnv(..))
import GHC.Tc.Utils.Monad (initTcInteractive, getGblEnv, setGblEnv, failM)
import GHC.Types.Name (Name)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Parser qualified as Parser
import GHC.Parser.Header (mkPrelImports)
import GHC.Parser.Lexer (ParseResult(..), initParserState, unP)
import GHC.Data.FastString (mkFastString)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Driver.Ppr (showPpr)
import GHC.Hs
import GHC.Types.Basic (Boxity (Boxed), DoPmc (SkipPmc), GenReason (OtherExpansion), Origin (Generated))
import GHC.Types.Name.Occurrence (mkVarOcc, occNameString, delFromOccEnv)
import GHC.Types.Name.Reader
  ( RdrName, isUnqual, mkRdrQual, mkRdrUnqual, rdrNameOcc
  , GlobalRdrEnv, mkGlobalRdrEnv, mkLocalVanillaGRE, Parent(NoParent)
  , plusGlobalRdrEnv, shadowNames, extendGlobalRdrEnv )
import GHC.Types.SrcLoc (GenLocated (L), Located, getLoc, unLoc, noSrcSpan, mkRealSrcLoc)
import GHC.Unit.Types (interactiveUnit, mkModule)
import GHC.Unit.Module (mkModuleName)
import GHC.Unit.Module.ModSummary (ms_hspp_opts)
import GHC.Utils.Outputable (ppr, text, (<+>))
import Tidepool.TypedSegment.Types
import Tidepool.ExactHydration
  ( CapturedGeneratedScaffoldSource, capturedGeneratedScaffoldBytes, capturedGeneratedScaffoldPath )
import Tidepool.QuasiQuoteOccurrences (quasiQuoteOccurrences)

-- The source issuer has already checked these bytes against its protected
-- recipe. This parser result is also the no-quasiquote proof required before
-- the demand renamer can load any imported interface.
data ParsedSegmentDemand = ParsedSegmentDemand
  CapturedGeneratedScaffoldSource DynFlags (Located (HsModule GhcPs))

segmentDemandMatchesSource :: ParsedSegmentDemand -> FilePath -> BS.ByteString -> Bool
segmentDemandMatchesSource (ParsedSegmentDemand captured _ _) path bytes =
  capturedGeneratedScaffoldPath captured == path && capturedGeneratedScaffoldBytes captured == bytes

prepareSegmentDemand :: TypedSegmentPlan -> DynFlags -> CapturedGeneratedScaffoldSource
  -> Either TypedSegmentFailure ParsedSegmentDemand
prepareSegmentDemand plan flags captured = case unP Parser.parseModule parser of
  PFailed _ -> Left UnprovedRootAbstraction
  POk _ parsed
    | null (quasiQuoteOccurrences flags parsed), roots parsed == [()] ->
        Right (ParsedSegmentDemand captured flags parsed)
    | otherwise -> Left UnprovedRootAbstraction
  where
    source = Text.unpack (TextEncoding.decodeUtf8 (capturedGeneratedScaffoldBytes captured))
    parser = initParserState (initParserOpts flags) (stringToStringBuffer source)
      (mkRealSrcLoc (mkFastString (capturedGeneratedScaffoldPath captured)) 1 1)
    roots parsed =
      [ () | L _ (ValD _ FunBind {fun_id=identifier}) <- hsmodDecls (unLoc parsed)
      , occNameString (rdrNameOcc (unLoc identifier)) == typedSegmentPlanRoot plan ]

-- Placeholder Names describe only the world's lexical aliases. Their exact
-- association stays with the supplied admitted binding; they never supply a type,
-- native identity or value certificate. A fixed private interactive module
-- lets NameCache share each alias across worlds without retaining per-cell
-- namespaces. The colon makes this qualifier impossible in authored syntax.
-- GHC gives each query's declarations their own module.
data SegmentDemandScope binding = SegmentDemandScope
  !GlobalRdrEnv !(Map.Map Name (String,binding))

newSegmentDemandScope :: HscEnv -> Map.Map String binding -> IO (SegmentDemandScope binding)
newSegmentDemandScope environment bindings = do
  entries <- forM (Map.toAscList bindings) $ \(alias,binding) -> do
    name <- allocateGlobalBinder (hsc_NC environment)
      (mkModule interactiveUnit (mkModuleName ":TidepoolDemand")) (mkVarOcc alias) noSrcSpan
    pure (name,(alias,binding))
  pure (SegmentDemandScope
    (mkGlobalRdrEnv [mkLocalVanillaGRE NoParent name | (name,_) <- entries])
    (Map.fromList entries))

-- The successor receiver supplies only authenticated changed aliases. The
-- alias-only GRE and exact reverse associations share every untouched branch;
-- stable NameCache names avoid a namespace per logical world.
updateSegmentDemandScope :: HscEnv -> [(String,Maybe binding)] -> SegmentDemandScope binding
  -> IO (SegmentDemandScope binding)
updateSegmentDemandScope environment changes initial = foldM update initial changes
  where
    update (SegmentDemandScope aliases bindings) (alias,replacement) = do
      let occurrence = mkVarOcc alias
      name <- allocateGlobalBinder (hsc_NC environment)
        (mkModule interactiveUnit (mkModuleName ":TidepoolDemand")) occurrence noSrcSpan
      let removed = delFromOccEnv aliases occurrence
          remaining = Map.delete name bindings
      pure $ case replacement of
        Nothing -> SegmentDemandScope removed remaining
        Just binding -> SegmentDemandScope
          (extendGlobalRdrEnv removed (mkLocalVanillaGRE NoParent name))
          (Map.insert name (alias,binding) remaining)

-- GHC owns the complete module and local lexical scopes, including class
-- defaults, instance methods, constructors and record fields. All imports
-- retain their real identities and qualification. The notebook alias GREs
-- shadow imported values through the same GHC operation used by GHCi; new
-- module declarations then shadow those aliases during rnSrcDecls.
--
-- The owning admission must validate source execution policy before entry and
-- install only its actually selected source interfaces. No compiler environment
-- escapes the probe, and genuine renamer errors are never discarded.
typedSegmentInputOccurrences :: HscEnv -> SegmentDemandScope binding -> ParsedSegmentDemand
  -> IO (Either TypedSegmentFailure [(String,binding)])
typedSegmentInputOccurrences environment (SegmentDemandScope aliases bindings)
    (ParsedSegmentDemand _ flags parsed) = do
  let probeFlags = gopt_set (gopt_unset flags Opt_HelpfulErrors) Opt_DeferOutOfScopeVariables
      probe = hscUpdateFlags (const probeFlags) environment
      syntax = unLoc parsed
      imports = hsmodImports syntax
      owner = maybe (mkModuleName "Main") unLoc (hsmodName syntax)
      allImports = mkPrelImports owner noSrcSpan (xopt LangExt.ImplicitPrelude flags) imports ++ imports
  (diagnostics,renamed) <- initTcInteractive probe $ do
    (_,_,imported,available,_,_) <- rnImports
      [(declaration,text "held-world demand") | declaration <- allImports]
    current <- getGblEnv
    let scope = plusGlobalRdrEnv aliases (shadowNames False imported aliases)
    setGblEnv (current {tcg_rdr_env=scope,tcg_imports=available}) $ do
      (group,splice) <- findSplice (hsmodDecls syntax)
      case splice of
        Just _ -> failM
        Nothing -> pure ()
      (_,result) <- rnSrcDecls group
      pure result
  pure $ case renamed of
    Nothing -> Left (InputDemandRenameRejected (showPpr probeFlags diagnostics))
    Just result ->
      let selected = everything (++) (mkQ [] occurrence) result
          demanded = Map.fromList
            [ (alias,(alias,binding))
            | name <- selected, Just (alias,binding) <- [Map.lookup name bindings] ]
      in Right (Map.elems demanded)
  where
    occurrence :: HsExpr GhcRn -> [Name]
    occurrence (HsVar _ (L _ name)) = [name]
    occurrence _ = []

rewriteParsedSegmentRoot :: GeneratedSegmentOperations -> TypedSegmentPlan -> ParsedModule -> IO ParsedModule
rewriteParsedSegmentRoot operations plan parsed = case rewriteModule of
  Left failure -> throwIO failure
  Right rewrittenSource -> pure (parsed { pm_parsed_source = rewrittenSource })
  where
    source = pm_parsed_source parsed
    authoredNames = parsedNames source
    generatedNames = concatMap plannedNames (typedSegmentPlanItems plan)
    collisions = generatedNames `intersect` authoredNames
    rewriteModule
      | length generatedNames /= length (nub generatedNames) = Left InvalidReservedNames
      | not (null collisions) = Left InvalidReservedNames
      | otherwise = case rewriteRoot operations (ms_hspp_opts (pm_mod_summary parsed)) plan source of
          Left failure -> Left failure
          Right (source', 1) -> Right source'
          Right _ -> Left UnprovedRootAbstraction

plannedNames :: TypedItemPlan -> [String]
plannedNames item = plannedItemEntry item : case plannedItemBody item of
  ActionItem step probe marker _ -> [step, probe, marker]
  LetItem marker _ -> [marker]
  ObservationItem probe observation -> [probe, observation, observation ++ "_unit"]

parsedNames :: Data source => source -> [String]
parsedNames = everything (++) (mkQ [] (\name -> [occNameString (rdrNameOcc name)] :: [String]))

rewriteRoot
  :: GeneratedSegmentOperations
  -> DynFlags
  -> TypedSegmentPlan
  -> Located (HsModule GhcPs)
  -> Either TypedSegmentFailure (Located (HsModule GhcPs), Int)
rewriteRoot operations flags plan (L moduleLocation hsModule) = do
  (declarations, matches) <- foldl rewriteDeclaration (Right ([], 0)) (hsmodDecls hsModule)
  pure (L moduleLocation (hsModule { hsmodDecls = reverse declarations }), matches)
  where
    root = typedSegmentPlanRoot plan
    rewriteDeclaration result declaration = do
      (rewritten, count) <- result
      case unLoc declaration of
        ValD extension binding@FunBind { fun_id = identifier }
          | occNameString (rdrNameOcc (unLoc identifier)) == root -> do
              binding' <- rewriteBinding binding
              pure (L (getLoc declaration) (ValD extension binding') : rewritten, count + 1)
        _ -> pure (declaration : rewritten, count)

    rewriteBinding binding@FunBind{} = case unLoc (mg_alts (fun_matches binding)) of
      [locatedMatch] -> do
        let match = unLoc locatedMatch
        case (unLoc (m_pats match), grhssGRHSs (m_grhss match)) of
          ([], [locatedRhs]) -> do
            let rhs = unLoc locatedRhs
            case (rhs, grhssLocalBinds (m_grhss match)) of
              (GRHS rhsExtension [] (L _ (HsDo _ (DoExpr Nothing) statements)), EmptyLocalBinds _) -> do
                body <- rewriteStatements operations flags plan (unLoc statements)
                let rhs' = GRHS rhsExtension [] body
                    grhss' = (m_grhss match) { grhssGRHSs = [L (getLoc locatedRhs) rhs'] }
                    match' = match { m_grhss = grhss' }
                    matches' = (fun_matches binding)
                      { mg_alts = noLocA [L (getLoc locatedMatch) match'] }
                pure (binding { fun_matches = matches' })
              _ -> Left UnprovedRootAbstraction
          _ -> Left UnprovedRootAbstraction
      _ -> Left UnprovedRootAbstraction

    rewriteBinding _ = Left UnprovedRootAbstraction

rewriteStatements
  :: GeneratedSegmentOperations
  -> DynFlags
  -> TypedSegmentPlan
  -> [ExprLStmt GhcPs]
  -> Either TypedSegmentFailure (LHsExpr GhcPs)
rewriteStatements operations flags plan statements = case reverse statements of
  [] -> Left UnprovedRootAbstraction
  terminal : reversedItems -> do
    final <- terminalExpression terminal
    let items = reverse reversedItems
        planned = typedSegmentPlanItems plan
    if length items /= length planned
      then Left (UnprovedItemSequence (min (length items) (length planned)))
      else if not (consecutiveOrdinals (map plannedItemOrdinal planned))
        then Left InvalidItemOrdinals
        else lowerItems (zip planned items) final
  where
    terminalExpression (L _ statement) = case statement of
      LastStmt _ expression _ _
        | isPlainPureUnit expression -> Right (segmentPureUnit operations)
      BodyStmt _ expression _ _
        | isPlainPureUnit expression -> Right (segmentPureUnit operations)
      _ -> Left UnprovedRootAbstraction

    lowerItems [] final = Right final
    lowerItems ((item, statement) : rest) final = do
      continuation <- lowerItems rest final
      lowerItem operations flags item statement continuation

consecutiveOrdinals :: [Int] -> Bool
consecutiveOrdinals [] = True
consecutiveOrdinals (_ : []) = True
consecutiveOrdinals (first : second : rest) =
  second == first + 1 && consecutiveOrdinals (second : rest)

lowerItem
  :: GeneratedSegmentOperations
  -> DynFlags
  -> TypedItemPlan
  -> ExprLStmt GhcPs
  -> LHsExpr GhcPs
  -> Either TypedSegmentFailure (LHsExpr GhcPs)
lowerItem operations flags item (L _ statement) continuation =
  case plannedItemBody item of
    ActionItem step probe marker expectedBinders -> case statement of
      BindStmt _ pattern rhs
        | binderNames pattern == expectedBinders ->
            Right (segmentBind operations (wrapActionRhs step probe rhs)
              (lazyLambda marker (patternCase operations flags marker pattern continuation)))
      _ -> Left (UnprovedItemSequence (plannedItemOrdinal item))
    LetItem marker expectedBinders -> case statement of
      LetStmt _ local
        | localBinderNames local == expectedBinders -> do
            local' <- addUnitMarker (plannedItemOrdinal item) marker local
            Right (noLocA (HsLet noAnn local' continuation))
      _ -> Left (UnprovedItemSequence (plannedItemOrdinal item))
    ObservationItem probe observation -> case statement of
      BodyStmt _ rhs _ _ ->
        let checkerCall = mkHsApp (lazyLambda probe
              (mkHsApp (unqualifiedVariable "__tidepoolCellExpression") (variable probe))) (mkLHsPar rhs)
            unitContinuation = lazyLambda (observation ++ "_unit") continuation
        in Right (segmentBind operations checkerCall unitContinuation)
      _ -> Left (UnprovedItemSequence (plannedItemOrdinal item))

binderNames :: LPat GhcPs -> [String]
binderNames = map (occNameString . rdrNameOcc) . collectPatBinders CollNoDictBinders

localBinderNames :: HsLocalBinds GhcPs -> [String]
localBinderNames = map (occNameString . rdrNameOcc) . collectLocalBinders CollNoDictBinders

wrapActionRhs :: String -> String -> LHsExpr GhcPs -> LHsExpr GhcPs
wrapActionRhs step probe rhs = identity step (identity probe rhs)
  where
    identity name expression = mkHsApp (lazyLambda name (variable name)) (mkLHsPar expression)

lazyLambda :: String -> LHsExpr GhcPs -> LHsExpr GhcPs
lazyLambda name = mkHsLam (noLocA [lazyVariablePattern name])

lazyVariablePattern :: String -> LPat GhcPs
lazyVariablePattern name = noLocA (LazyPat noAnn (nlVarPat (unqualifiedName name)))

patternCase :: GeneratedSegmentOperations -> DynFlags -> String -> LPat GhcPs -> LHsExpr GhcPs -> LHsExpr GhcPs
patternCase operations flags valueName originalPattern continuation = noLocA
  (HsCase noAnn (variable valueName) alternatives)
  where
    sourceLoc = getLocA originalPattern
    failedPattern = L (getLoc originalPattern) (WildPat noExtField)
    message = showPpr flags
      (text "Pattern match failure in" <+> pprHsDoFlavour (DoExpr Nothing)
        <+> text "at" <+> ppr sourceLoc)
    failure = mkHsApps (qualifiedVariable operations "segmentFail")
      [noLocA (HsLit noExtField (mkHsString message))]
    alternatives = mkMatchGroup (Generated OtherExpansion SkipPmc)
      (noLocA [mkHsCaseAlt originalPattern continuation, mkHsCaseAlt failedPattern failure])

segmentBind :: GeneratedSegmentOperations -> LHsExpr GhcPs -> LHsExpr GhcPs -> LHsExpr GhcPs
-- GHC's parsed-expression printer preserves grouping through HsPar nodes.
-- Generated applications must retain those nodes around argument expressions.
segmentBind operations action continuation = mkHsApps (qualifiedVariable operations "segmentBind")
  [mkLHsPar action, mkLHsPar continuation]

segmentPureUnit :: GeneratedSegmentOperations -> LHsExpr GhcPs
segmentPureUnit operations = mkHsApp (qualifiedVariable operations "segmentPure") unitExpression

unitExpression :: LHsExpr GhcPs
unitExpression = noLocA (ExplicitTuple noAnn [] Boxed)

isPlainPureUnit :: LHsExpr GhcPs -> Bool
isPlainPureUnit expression = case unLoc expression of
  HsPar _ inner -> isPlainPureUnit inner
  HsApp _ function argument -> isPureName function && isUnitExpression argument
  _ -> False
  where
    isPureName function = case unLoc function of
      HsVar _ name -> isUnqual (unLoc name)
        && occNameString (rdrNameOcc (unLoc name)) == "pure"
      _ -> False
    isUnitExpression value = case unLoc value of
      HsPar _ inner -> isUnitExpression inner
      ExplicitTuple _ [] Boxed -> True
      HsVar _ name -> occNameString (rdrNameOcc (unLoc name)) == "()"
      _ -> False

unqualifiedName :: String -> RdrName
unqualifiedName = mkRdrUnqual . mkVarOcc

unqualifiedVariable :: String -> LHsExpr GhcPs
unqualifiedVariable = nlHsVar . unqualifiedName

qualifiedVariable :: GeneratedSegmentOperations -> String -> LHsExpr GhcPs
qualifiedVariable operations name = nlHsVar
  (mkRdrQual (generatedSegmentQualifier operations) (mkVarOcc name))

variable :: String -> LHsExpr GhcPs
variable = unqualifiedVariable

addUnitMarker :: Int -> String -> HsLocalBinds GhcPs -> Either TypedSegmentFailure (HsLocalBinds GhcPs)
addUnitMarker ordinal marker local = case local of
  EmptyLocalBinds _ -> Right (HsValBinds noAnn
    (ValBinds NoAnnSortKey [markerBinding] []))
  HsValBinds extension binds -> Right (HsValBinds extension
    (plusHsValBinds binds (ValBinds NoAnnSortKey [markerBinding] [])))
  HsIPBinds {} -> Left (UnprovedItemSequence ordinal)
  where
    markerBinding = mkHsVarBind noSrcSpan (unqualifiedName marker) unitExpression
