-- | Parsed-source lowering for compiler-owned typed effect segments.
-- Authored nested do expressions remain opaque to this rewrite.
module Tidepool.TypedSegment.Source
  ( rewriteParsedSegmentRoot
  ) where

import Control.Exception (throwIO)
import Data.Data (Data)
import Data.Generics (everything, mkQ)
import Data.List (intersect, nub)
import GHC (ParsedModule (..), GhcPs)
import GHC.Hs
import GHC.Hs.Utils
import GHC.Parser.Annotation (noLocA, noAnn)
import GHC.Types.Basic (Boxity (Boxed), DoPmc (SkipPmc), GenReason (OtherExpansion), Origin (Generated))
import GHC.Types.Name.Occurrence (mkVarOcc, occNameString)
import GHC.Types.Name.Reader (RdrName, mkRdrQual, mkRdrUnqual, rdrNameOcc)
import GHC.Types.SrcLoc (noSrcSpan)
import GHC.Unit.Module (mkModuleName)
import GHC.Utils.Outputable (defaultSDocContext, ppr, showSDocOneLine)
import Language.Haskell.Syntax.Extension (noExtField)
import Tidepool.TypedSegment.Types

rewriteParsedSegmentRoot :: TypedSegmentPlan -> ParsedModule -> IO ParsedModule
rewriteParsedSegmentRoot plan parsed = case rewriteModule of
  Left failure -> throwIO failure
  Right source -> pure (parsed { pm_parsed_source = source })
  where
    source = pm_parsed_source parsed
    authoredNames = parsedNames source
    generatedNames = concatMap plannedNames (typedSegmentPlanItems plan)
    collisions = generatedNames `intersect` authoredNames
    rewriteModule
      | length generatedNames /= length (nub generatedNames) = Left InvalidReservedNames
      | not (null collisions) = Left InvalidReservedNames
      | otherwise = case rewriteRoot plan source of
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
  :: TypedSegmentPlan
  -> LHsModule GhcPs
  -> Either TypedSegmentFailure (LHsModule GhcPs, Int)
rewriteRoot plan (L moduleLocation hsModule) = do
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

    rewriteBinding binding = case unLoc (mg_alts (fun_matches binding)) of
      [locatedMatch] -> do
        let match = unLoc locatedMatch
        case (unLoc (m_pats match), grhssGRHSs (m_grhss match)) of
          ([], [locatedRhs]) -> do
            let rhs = unLoc locatedRhs
            case (grhs_guard rhs, unLoc (grhs_body rhs)) of
              ([], HsDo _ DoExpr statements) -> do
                body <- rewriteStatements plan (unLoc statements)
                let rhs' = rhs { grhs_body = noLocA body }
                    grhss' = (m_grhss match) { grhssGRHSs = [locatedRhs { unLoc = rhs' }] }
                    match' = match { m_grhss = grhss' }
                    matches' = (fun_matches binding)
                      { mg_alts = noLocA [locatedMatch { unLoc = match' }] }
                pure (binding { fun_matches = matches' })
              _ -> Left UnprovedRootAbstraction
          _ -> Left UnprovedRootAbstraction
      _ -> Left UnprovedRootAbstraction

rewriteStatements
  :: TypedSegmentPlan
  -> [ExprLStmt GhcPs]
  -> Either TypedSegmentFailure (LHsExpr GhcPs)
rewriteStatements plan statements = case reverse statements of
  [] -> Left UnprovedRootAbstraction
  terminal : reversedItems -> do
    final <- terminalExpression terminal
    let items = reverse reversedItems
        planned = typedSegmentPlanItems plan
    if length items /= length planned
      then Left (UnprovedItemSequence (min (length items) (length planned)))
      else if map plannedItemOrdinal planned /= [0 .. length planned - 1]
        then Left InvalidItemOrdinals
        else lowerItems (zip planned items) final
  where
    terminalExpression (L _ statement) = case statement of
      LastStmt _ expression _ _
        | isPlainPureUnit expression -> Right segmentPureUnit
      _ -> Left UnprovedRootAbstraction

    lowerItems [] final = Right final
    lowerItems ((item, statement) : rest) final = do
      continuation <- lowerItems rest final
      lowerItem item statement continuation

lowerItem
  :: TypedItemPlan
  -> ExprLStmt GhcPs
  -> LHsExpr GhcPs
  -> Either TypedSegmentFailure (LHsExpr GhcPs)
lowerItem item (L _ statement) continuation =
  case plannedItemBody item of
    ActionItem step probe marker expectedBinders -> case statement of
      BindStmt _ pattern rhs
        | binderNames pattern == expectedBinders ->
            Right (segmentBind (wrapActionRhs step probe rhs)
              (lazyLambda marker (patternCase marker pattern continuation)))
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
              (mkHsApp (unqualifiedVariable "__tidepoolCellExpression") (variable probe))) rhs
            unitContinuation = lazyLambda (observation ++ "_unit") continuation
        in Right (segmentBind checkerCall unitContinuation)
      _ -> Left (UnprovedItemSequence (plannedItemOrdinal item))

binderNames :: LPat GhcPs -> [String]
binderNames = map (occNameString . rdrNameOcc) . collectPatBinders CollNoDictBinders

localBinderNames :: HsLocalBinds GhcPs -> [String]
localBinderNames = map (occNameString . rdrNameOcc) . collectLocalBinders CollNoDictBinders

wrapActionRhs :: String -> String -> LHsExpr GhcPs -> LHsExpr GhcPs
wrapActionRhs step probe rhs = identity step (identity probe rhs)
  where
    identity name expression = mkHsApp (lazyLambda name (variable name)) expression

lazyLambda :: String -> LHsExpr GhcPs -> LHsExpr GhcPs
lazyLambda name = mkHsLam (noLocA [lazyVariablePattern name])

lazyVariablePattern :: String -> LPat GhcPs
lazyVariablePattern name = noLocA (LazyPat noAnn (nlVarPat (unqualifiedName name)))

patternCase :: String -> LPat GhcPs -> LHsExpr GhcPs -> LHsExpr GhcPs
patternCase valueName originalPattern continuation = noLocA
  (HsCase noExtField (variable valueName) alternatives)
  where
    sourceLoc = getLocA originalPattern
    failedPattern = L sourceLoc (WildPat noExtField)
    message = "Pattern match failure in do expression at "
      ++ showSDocOneLine defaultSDocContext (ppr (locA sourceLoc))
    failure = mkHsApps (qualifiedVariable "segmentFail")
      [noLocA (HsLit noExtField (mkHsString message))]
    alternatives = mkMatchGroup (Generated OtherExpansion SkipPmc)
      (noLocA [mkHsCaseAlt originalPattern continuation, mkHsCaseAlt failedPattern failure])

segmentBind :: LHsExpr GhcPs -> LHsExpr GhcPs -> LHsExpr GhcPs
segmentBind action continuation = mkHsApps (qualifiedVariable "segmentBind") [action, continuation]

segmentPureUnit :: LHsExpr GhcPs
segmentPureUnit = mkHsApp (qualifiedVariable "segmentPure") unitExpression

unitExpression :: LHsExpr GhcPs
unitExpression = noLocA (ExplicitTuple noAnn [] Boxed)

isPlainPureUnit :: LHsExpr GhcPs -> Bool
isPlainPureUnit expression = case unLoc expression of
  HsPar _ inner -> isPlainPureUnit inner
  HsApp _ function argument -> isPureName function && isUnitExpression argument
  _ -> False
  where
    isPureName function = case unLoc function of
      HsVar _ name -> occNameString (rdrNameOcc (unLoc name)) == "pure"
      _ -> False
    isUnitExpression value = case unLoc value of
      HsPar _ inner -> isUnitExpression inner
      ExplicitTuple _ [] Boxed -> True
      _ -> False

unqualifiedName :: String -> RdrName
unqualifiedName = mkRdrUnqual . mkVarOcc

unqualifiedVariable :: String -> LHsExpr GhcPs
unqualifiedVariable = nlHsVar . unqualifiedName

qualifiedVariable :: String -> LHsExpr GhcPs
qualifiedVariable name = nlHsVar
  (mkRdrQual (mkModuleName "TidepoolResume") (mkVarOcc name))

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
