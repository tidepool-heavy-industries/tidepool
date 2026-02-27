module Tidepool.Translate
  ( translateBinds
  , translateModule
  , translateModuleClosed
  , collectDataCons
  , collectUsedDataCons
  , collectTransitiveDCons
  , wiredInDataCons
  , dcToMeta
  , valueRepArity
  , mapBang
  , FlatNode(..)
  , FlatAlt(..)
  , FlatAltCon(..)
  , LitEnc(..)
  , UnresolvedVar(..)
  ) where

import Debug.Trace (trace)
import GHC
import GHC.Core
import GHC.Types.Id
import GHC.Types.Var (isTyVar, varUnique, varName)
import GHC.Types.Unique (getKey)
import GHC.Core.DataCon (DataCon, dataConRepArity, dataConRepArgTys, dataConFullSig, dataConTag, dataConWorkId, dataConName, dataConSrcBangs, dataConOrigArgTys, isUnboxedTupleDataCon, HsSrcBang(..), HsBang(..), SrcUnpackedness(..), SrcStrictness(..))
import Language.Haskell.Syntax.Basic (Boxity(..))
import GHC.Builtin.Types (consDataCon, nilDataCon, trueDataCon, falseDataCon, charDataCon, unitDataCon, tupleDataCon, ordLTDataCon, ordEQDataCon, ordGTDataCon, intDataCon, wordDataCon, doubleDataCon, floatDataCon)
import GHC.Builtin.PrimOps
import GHC.Types.Literal
import GHC.Types.Name (nameOccName, isExternalName, isSystemName, nameModule_maybe)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Unit.Module (moduleName, moduleNameString)
import GHC.Utils.Fingerprint (fingerprintString, Fingerprint(..))
import GHC.Core.TyCon
import GHC.Core.Type (splitTyConApp_maybe, isCoercionTy)
import GHC.Builtin.Types.Prim (statePrimTyCon)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.TyCo.FVs (tyConsOfType)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Types.Var.Set (VarSet, emptyVarSet, extendVarSet, elemVarSet)
import GHC.Types.Unique.Set as USet (nonDetEltsUniqSet)
import GHC.Types.Unique.Set (UniqSet, emptyUniqSet, addOneToUniqSet, elementOfUniqSet, nonDetEltsUniqSet)
import GHC.Types.Basic (JoinPointHood(..))
import GHC.Utils.Outputable (showPprUnsafe)
import GHC.Float (castDoubleToWord64, castFloatToWord32)
import Data.Char (ord)
import Data.List (isPrefixOf, isInfixOf)
import Data.Bits ((.&.), (.|.), shiftL, shiftR)
import Data.Word
import Data.Int
import Data.Text (Text)
import qualified Data.Set as Set
import qualified Data.Text as T
import Data.ByteString (ByteString)
import qualified Data.ByteString as BS
import Data.Sequence (Seq, (|>))
import qualified Data.Sequence as Seq
import qualified Data.Map.Strict as Map
import Control.Monad.State
import Control.Monad (foldM, forM, when)
import System.IO (hPutStrLn, stderr)
import Numeric (showHex)

import GHC.Driver.Env (HscEnv)
import Tidepool.Resolve (resolveExternals, UnresolvedVar(..))

data FlatNode
  = NVar !Word64
  | NLit !LitEnc
  | NApp !Int !Int
  | NLam !Word64 !Int
  | NLetNonRec !Word64 !Int !Int
  | NLetRec ![(Word64, Int)] !Int
  | NCase !Int !Word64 ![FlatAlt]
  | NCon !Word64 ![Int]
  | NJoin !Word64 ![Word64] !Int !Int
  | NJump !Word64 ![Int]
  | NPrimOp !Text ![Int]
  deriving (Show)

data FlatAlt = FlatAlt !FlatAltCon ![Word64] !Int
  deriving (Show)

data FlatAltCon = FDataAlt !Word64 | FLitAlt !LitEnc | FDefault
  deriving (Show)

data LitEnc
  = LEInt !Int64
  | LEWord !Word64
  | LEChar !Word32
  | LEString !ByteString
  | LEFloat !Word64    -- IEEE 754 bits
  | LEDouble !Word64   -- IEEE 754 bits
  deriving (Show)

data TransState = TransState
  { tsNodes :: !(Seq FlatNode)
  , tsUsedDCs :: !(Map.Map Word64 DataCon)
  , tsRecJoinIds :: !(Set.Set Word64)  -- join IDs from Rec groups (translated as LetRec lambdas)
  , tsSynthCounter :: !Word64          -- counter for synthetic VarIds (tag 'T')
  , tsUnresolvedIds :: !(Set.Set Word64) -- IDs that should be translated as error nodes
  }

type TransM = State TransState

emitNode :: FlatNode -> TransM Int
emitNode n = do
  s <- get
  let idx = Seq.length (tsNodes s)
  put s { tsNodes = tsNodes s |> n }
  return idx

-- | Generate a fresh synthetic VarId with tag 'T' (Tidepool-generated).
freshSynthVarId :: TransM Word64
freshSynthVarId = do
  s <- get
  let c = tsSynthCounter s
  put s { tsSynthCounter = c + 1 }
  -- Tag 'T' = 0x54, shifted left 56 bits
  return (0x5400000000000000 .|. c)

recordDC :: DataCon -> TransM ()
recordDC dc = modify' $ \s ->
  s { tsUsedDCs = Map.insert (varId (dataConWorkId dc)) dc (tsUsedDCs s) }

-- | Emit a runtime unpackCString# loop for a non-static Addr# value.
-- Produces: letrec go = \a -> case indexCharOffAddr# a 0# of
--             { '\0'# -> []; c -> C# c : go (plusAddr# a 1#) }
--           in go addrIdx
emitRuntimeUnpackCString :: Int -> TransM Int
emitRuntimeUnpackCString addrIdx = do
    goId <- freshSynthVarId
    aId <- freshSynthVarId
    let consId = varId (dataConWorkId consDataCon)
        nilId  = varId (dataConWorkId nilDataCon)
        charId = varId (dataConWorkId charDataCon)
    recordDC consDataCon
    recordDC nilDataCon
    recordDC charDataCon
    -- Body: indexCharOffAddr# a 0#
    aRef <- emitNode $ NVar aId
    lit0 <- emitNode $ NLit (LEInt 0)
    charAt <- emitNode $ NPrimOp (T.pack "IndexCharOffAddr") [aRef, lit0]
    -- [] (nil result)
    nilIdx <- emitNode $ NCon nilId []
    -- C# charAt : go (plusAddr# a 1#)
    charBox <- emitNode $ NCon charId [charAt]
    lit1 <- emitNode $ NLit (LEInt 1)
    aRef2 <- emitNode $ NVar aId
    nextAddr <- emitNode $ NPrimOp (T.pack "PlusAddr") [aRef2, lit1]
    goRef <- emitNode $ NVar goId
    goNext <- emitNode $ NApp goRef nextAddr
    consResult <- emitNode $ NCon consId [charBox, goNext]
    -- case charAt of { '\0'# -> []; DEFAULT -> consResult }
    let nullAlt = FlatAlt (FLitAlt (LEChar 0)) [] nilIdx
        defaultAlt = FlatAlt FDefault [] consResult
    caseIdx <- emitNode $ NCase charAt 0 [nullAlt, defaultAlt]
    -- \a -> case ...
    lamA <- emitNode $ NLam aId caseIdx
    -- go addrIdx
    goRef2 <- emitNode $ NVar goId
    appIdx <- emitNode $ NApp goRef2 addrIdx
    -- letrec go = \a -> ... in go addrIdx
    emitNode $ NLetRec [(goId, lamA)] appIdx

-- | Emit a runtime unpackAppendCString# loop for a non-static Addr# value.
-- Like emitRuntimeUnpackCString but appends suffix instead of []:
-- letrec go = \a -> case indexCharOffAddr# a 0# of
--           { '\0'# -> suffix; c -> C# c : go (plusAddr# a 1#) }
--         in go addrIdx
emitRuntimeUnpackAppendCString :: Int -> Int -> TransM Int
emitRuntimeUnpackAppendCString addrIdx suffixIdx = do
    goId <- freshSynthVarId
    aId <- freshSynthVarId
    let consId = varId (dataConWorkId consDataCon)
        charId = varId (dataConWorkId charDataCon)
    recordDC consDataCon
    recordDC charDataCon
    -- Body: indexCharOffAddr# a 0#
    aRef <- emitNode $ NVar aId
    lit0 <- emitNode $ NLit (LEInt 0)
    charAt <- emitNode $ NPrimOp (T.pack "IndexCharOffAddr") [aRef, lit0]
    -- C# charAt : go (plusAddr# a 1#)
    charBox <- emitNode $ NCon charId [charAt]
    lit1 <- emitNode $ NLit (LEInt 1)
    aRef2 <- emitNode $ NVar aId
    nextAddr <- emitNode $ NPrimOp (T.pack "PlusAddr") [aRef2, lit1]
    goRef <- emitNode $ NVar goId
    goNext <- emitNode $ NApp goRef nextAddr
    consResult <- emitNode $ NCon consId [charBox, goNext]
    -- case charAt of { '\0'# -> suffix; DEFAULT -> consResult }
    let nullAlt = FlatAlt (FLitAlt (LEChar 0)) [] suffixIdx
        defaultAlt = FlatAlt FDefault [] consResult
    caseIdx <- emitNode $ NCase charAt 0 [nullAlt, defaultAlt]
    -- \a -> case ...
    lamA <- emitNode $ NLam aId caseIdx
    -- go addrIdx
    goRef2 <- emitNode $ NVar goId
    appIdx <- emitNode $ NApp goRef2 addrIdx
    -- letrec go = \a -> ... in go addrIdx
    emitNode $ NLetRec [(goId, lamA)] appIdx

translateBinds :: [CoreBind] -> [(String, Seq FlatNode)]
translateBinds binds = concatMap translateBind binds
  where
    translateBind (NonRec b rhs) =
      let (idx, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0 Set.empty)
          finalNodes = tsNodes s
          rootIdx = Seq.length finalNodes - 1
      in if idx == rootIdx
         then [(occNameString (nameOccName (idName b)), finalNodes)]
         else error "Root index mismatch in NonRec"
    translateBind (Rec pairs) =
      map (\(b, rhs) ->
        let (idx, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0 Set.empty)
            finalNodes = tsNodes s
            rootIdx = Seq.length finalNodes - 1
        in if idx == rootIdx
           then (occNameString (nameOccName (idName b)), finalNodes)
           else error "Root index mismatch in Rec"
      ) pairs

-- | Translate an entire module's bindings into a single self-contained tree.
-- All bindings become nested Let expressions wrapping a Var reference to the
-- target binding. This eliminates cross-binding Var references since all
-- definitions share one flat node array.
translateModule :: [CoreBind] -> String -> Set.Set Word64 -> (Seq FlatNode, Map.Map Word64 DataCon)
translateModule allBinds targetName unresolvedIds =
  let targetId = findTargetId targetName allBinds
      neededBinds = reachableBinds allBinds targetId
      (_, finalState) = runState (wrapAllBinds neededBinds targetId) (TransState Seq.empty Map.empty Set.empty 0 unresolvedIds)
  in (tsNodes finalState, tsUsedDCs finalState)
  where
    findTargetId name binds =
      case filter isTarget (concatMap bindersOf binds) of
        (b:_) -> b
        []    -> error $ "translateModule: exported top-level binding '" ++ name ++ "' not found"
      where
        isTarget b =
          occNameString (nameOccName (idName b)) == name
          && isExportedId b
          && isExternalName (idName b)
          && not (isSystemName (idName b))

    bindersOf (NonRec b _) = [b]
    bindersOf (Rec pairs)  = map fst pairs

    -- | Skip GHC-generated metadata bindings ($trModule, $krep, $tc*).
    -- These are Typeable / module-info bindings that reference unpackCString#
    -- and are never needed at runtime. Worker-wrappers ($w*) and
    -- specializations ($s*) must be kept.
    isMetadataBinder :: Id -> Bool
    isMetadataBinder b =
      let name = occNameString (nameOccName (idName b))
      in any (`isPrefixOf` name) ["$trModule", "$krep", "$tc"]

    -- | Filter bindings to only those transitively reachable from the target.
    -- Flattens Rec groups into individual (binder, rhs) pairs for fine-grained
    -- reachability analysis, then reconstructs reachable pairs into a single Rec.
    -- This prevents a single large Rec from pulling in all bindings when only
    -- a few are actually needed.
    reachableBinds :: [CoreBind] -> Id -> [CoreBind]
    reachableBinds binds target =
      let -- Flatten all binding groups into individual (binder, rhs) pairs
          allPairs :: [(Id, CoreExpr)]
          allPairs = concatMap (\bind -> case bind of
            NonRec b rhs -> [(b, rhs)]
            Rec ps       -> ps) binds

          -- Index each pair individually
          pairInfo :: [((Id, CoreExpr), Word64, Set.Set Word64)]
          pairInfo = map (\p@(b, rhs) ->
            (p, varId b, exprFreeVarKeys rhs)) allPairs

          -- Map from binder key -> index into pairInfo
          keyToIdx :: Map.Map Word64 Int
          keyToIdx = Map.fromList
            [(k, i) | (i, (_, k, _)) <- zip [0..] pairInfo]

          -- DFS collecting reachable pair indices
          go :: Set.Set Int -> [Word64] -> Set.Set Int
          go visited [] = visited
          go visited (v:vs) = case Map.lookup v keyToIdx of
            Just idx | not (Set.member idx visited) ->
              let (_, _, fvs) = pairInfo !! idx
              in go (Set.insert idx visited) (Set.toList fvs ++ vs)
            _ -> go visited vs

          targetKey = varId target
          reachable = case Map.lookup targetKey keyToIdx of
            Just idx ->
              let (_, _, fvs) = pairInfo !! idx
              in go (Set.singleton idx) (Set.toList fvs)
            Nothing -> Set.empty

          reachablePairs = [(b, rhs) | (i, ((b, rhs), _, _)) <- zip [0..] pairInfo, Set.member i reachable]
      in if null reachablePairs then [] else [Rec reachablePairs]

    -- | Extract free variable keys (as Word64) from a Core expression.
    exprFreeVarKeys :: CoreExpr -> Set.Set Word64
    exprFreeVarKeys expr =
      let fvs = exprSomeFreeVars (const True) expr
      in Set.fromList [varId v | v <- nonDetEltsUniqSet fvs]

    wrapAllBinds :: [CoreBind] -> Id -> TransM Int
    wrapAllBinds [] target = emitNode (NVar (varId target))
    wrapAllBinds (NonRec b rhs : rest) target
      | isTyVar b = wrapAllBinds rest target  -- skip type bindings
      | otherwise = do
          rhsIdx <- translate rhs
          bodyIdx <- wrapAllBinds rest target
          emitNode (NLetNonRec (varId b) rhsIdx bodyIdx)
    wrapAllBinds (Rec pairs : rest) target = do
      let valPairs = filter (\(b, _) -> not (isTyVar b)) pairs
      if null valPairs
        then wrapAllBinds rest target
        else do
          -- Register rec join IDs so call sites emit App instead of Jump
          let recJoins = [varId b | (b, _) <- valPairs, isJoinId b]
          modify' $ \s -> s { tsRecJoinIds = tsRecJoinIds s `Set.union` Set.fromList recJoins }
          pairIdxs <- forM valPairs $ \(b, rhs) -> do
            rhs' <- case isJoinId_maybe b of
              Just arity -> do
                let (params, joinBody) = collectValueBinders arity rhs
                joinBodyIdx <- translate joinBody
                foldM (\inner p -> emitNode $ NLam (varId p) inner)
                      joinBodyIdx (reverse params)
              Nothing -> translate rhs
            return (varId b, rhs')
          bodyIdx <- wrapAllBinds rest target
          emitNode (NLetRec pairIdxs bodyIdx)

-- | Like translateModule, but first resolves cross-module references
-- by inlining unfoldings from the GHC environment. Returns the
-- translated tree, used DataCons, and any variables that could not
-- be resolved (no unfolding available).
translateModuleClosed :: HscEnv -> [CoreBind] -> String -> IO (Seq FlatNode, Map.Map Word64 DataCon, [UnresolvedVar], [CoreBind])
translateModuleClosed hscEnv allBinds targetName = do
  (closedBinds, unresolved) <- resolveExternals hscEnv allBinds
  let unresolvedIds = Set.fromList (map uvKey unresolved)
      (nodes, usedDCs) = translateModule closedBinds targetName unresolvedIds
      referencedIds = foldl' (\acc n -> case n of { NVar v -> Set.insert v acc; _ -> acc }) Set.empty nodes
      trulyUnresolved = filter (\uv -> uvKey uv `Set.member` referencedIds) unresolved
      -- Debug: find dangling NVar references (referenced but not bound by any Let/Lam/Case)
      boundIds = foldl' collectBound Set.empty nodes
      danglingIds = Set.filter (\v -> not (Set.member v boundIds) && (v `shiftR` 56) /= 0x45) referencedIds
  -- Debug: find and report dangling NVar references
  let allVarRefs = concatMap deepVarRefsOfCB closedBinds
      varRefMap = Map.fromList [(varId v, v) | v <- allVarRefs]
  when (not (Set.null danglingIds)) $ do
    let nameMap = Map.fromList [(varId b, showPprUnsafe b) | b <- concatMap bindersOfCB closedBinds]
    hPutStrLn stderr $ "  [DEBUG] Dangling NVar references (" ++ show (Set.size danglingIds) ++ "):"
    mapM_ (\v -> do
      let tag = toEnum (fromIntegral (v `shiftR` 56)) :: Char
          key = v .&. ((1 `shiftL` 56) - 1)
          name = Map.findWithDefault "<unknown>" v nameMap
          refInfo = case Map.lookup v varRefMap of
            Just var -> showPprUnsafe var ++ " :: " ++ showPprUnsafe (idType var)
                     ++ case nameModule_maybe (varName var) of
                          Just m  -> " [" ++ moduleNameString (moduleName m) ++ "]"
                          Nothing -> " [no module]"
            Nothing -> "<?>"
      hPutStrLn stderr $ "    VarId(0x" ++ showHex v (") tag='" ++ [tag] ++ "' key=" ++ show key ++ " name=" ++ name ++ " ref=" ++ refInfo)
      ) (Set.toList danglingIds)
  return (nodes, usedDCs, trulyUnresolved, closedBinds)
  where
    collectBound :: Set.Set Word64 -> FlatNode -> Set.Set Word64
    collectBound acc (NLam b _) = Set.insert b acc
    collectBound acc (NLetNonRec b _ _) = Set.insert b acc
    collectBound acc (NLetRec pairs _) = foldl' (\a (b,_) -> Set.insert b a) acc pairs
    collectBound acc (NCase _ b alts) =
      let withBinder = Set.insert b acc
      in foldl' (\a (FlatAlt _ bs _) -> foldl' (\a' b' -> Set.insert b' a') a bs) withBinder alts
    collectBound acc (NJoin b params _ _) = foldl' (\a p -> Set.insert p a) (Set.insert b acc) params
    collectBound acc _ = acc
    bindersOfCB (NonRec b _) = [b]
    bindersOfCB (Rec pairs)  = map fst pairs
    -- Walk into all expressions to find ALL variable references (for debug naming)
    deepVarRefsOfCB :: CoreBind -> [Id]
    deepVarRefsOfCB (NonRec _ rhs) = deepVarRefsOfExpr rhs
    deepVarRefsOfCB (Rec pairs) = concatMap (deepVarRefsOfExpr . snd) pairs
    deepVarRefsOfExpr :: CoreExpr -> [Id]
    deepVarRefsOfExpr (Var v) = [v]
    deepVarRefsOfExpr (Lit _) = []
    deepVarRefsOfExpr (App f a) = deepVarRefsOfExpr f ++ deepVarRefsOfExpr a
    deepVarRefsOfExpr (Lam _ e) = deepVarRefsOfExpr e
    deepVarRefsOfExpr (Let bind e) = deepVarRefsOfCB bind ++ deepVarRefsOfExpr e
    deepVarRefsOfExpr (Case scrut _ _ alts) =
      deepVarRefsOfExpr scrut ++ concatMap (\(Alt _ _ rhs) -> deepVarRefsOfExpr rhs) alts
    deepVarRefsOfExpr (Cast e _) = deepVarRefsOfExpr e
    deepVarRefsOfExpr (Tick _ e) = deepVarRefsOfExpr e
    deepVarRefsOfExpr (Type _) = []
    deepVarRefsOfExpr (Coercion _) = []

-- | Collect all DataCons encountered during translation of Core bindings.
-- This includes constructors from imported packages (e.g. freer-simple's
-- Val, E, Leaf, Node, Union) that aren't in the module's mg_tcs.
collectUsedDataCons :: [CoreBind] -> [(Word64, Text, Int, Int, [Text])]
collectUsedDataCons binds =
  let allDCs = foldMap collectFromBind binds
  in map dcToMeta (Map.elems allDCs)
  where
    collectFromBind (NonRec _ rhs) =
      let (_, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0 Set.empty)
      in tsUsedDCs s
    collectFromBind (Rec pairs) =
      foldMap (\(_, rhs) ->
        let (_, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0 Set.empty)
        in tsUsedDCs s
      ) pairs

dcToMeta :: DataCon -> (Word64, Text, Int, Int, [Text])
dcToMeta dc =
  ( varId (dataConWorkId dc)
  , T.pack (occNameString (nameOccName (dataConName dc)))
  , dataConTag dc
  , valueRepArity dc
  , map mapBang (dataConSrcBangs dc)
  )

-- | Compute transitive closure of TyCons reachable from all binder types,
-- expanding through newtypes, then return metadata for all their DataCons.
collectTransitiveDCons :: [CoreBind] -> [(Word64, Text, Int, Int, [Text])]
collectTransitiveDCons binds =
  let binderTypes = [ idType b | b <- concatMap bindersOfBind binds ]
      seedTyCons  = foldMap (nonDetEltsUniqSet . tyConsOfType) binderTypes
      allTyCons   = closeTyCons emptyUniqSet seedTyCons
  in  concatMap tyConToDCMeta (nonDetEltsUniqSet allTyCons)
  where
    bindersOfBind (NonRec b _) = [b]
    bindersOfBind (Rec pairs)  = map fst pairs

closeTyCons :: UniqSet TyCon -> [TyCon] -> UniqSet TyCon
closeTyCons visited []     = visited
closeTyCons visited (tc:rest)
  | tc `elementOfUniqSet` visited = closeTyCons visited rest
  | otherwise =
      let visited' = addOneToUniqSet visited tc
          newtypeChildren = case unwrapNewTyCon_maybe tc of
            Just (_tvs, reprTy, _coax) -> nonDetEltsUniqSet (tyConsOfType reprTy)
            Nothing                    -> []
          fieldChildren = case tyConDataCons_maybe tc of
            Just dcs -> [ ftc
                        | dc <- dcs
                        , Scaled _ ft <- dataConOrigArgTys dc
                        , ftc <- nonDetEltsUniqSet (tyConsOfType ft) ]
            Nothing  -> []
      in closeTyCons visited' (newtypeChildren ++ fieldChildren ++ rest)

tyConToDCMeta :: TyCon -> [(Word64, Text, Int, Int, [Text])]
tyConToDCMeta tc = case tyConDataCons_maybe tc of
  Just dcs -> map (\dc ->
    ( varId (dataConWorkId dc)
    , T.pack (occNameString (nameOccName (dataConName dc)))
    , dataConTag dc
    , valueRepArity dc
    , map mapBang (dataConSrcBangs dc)
    )) dcs
  Nothing  -> []

translate :: CoreExpr -> TransM Int
translate expr =
  let (hd, allArgs) = collectArgs expr
      args = filter isValueArg allArgs
  in case hd of
    -- Desugar unpackCString#/unpackCStringUtf8# to cons-cell chain:
    -- GHC represents string literals as (unpackCString# "addr"#) in Core.
    -- We expand to (:) 'c1' ((:) 'c2' ... []) so strings are uniform [Char]
    -- cons cells, enabling case matching and (++) to work correctly.
    Var v | isUnpackCStringVar v
          , [arg] <- args
          , Just bytes <- extractAddrLitBytes arg -> do
        let consId = varId (dataConWorkId consDataCon)
            nilId  = varId (dataConWorkId nilDataCon)
            charId = varId (dataConWorkId charDataCon)
        recordDC consDataCon
        recordDC nilDataCon
        recordDC charDataCon
        nilIdx <- emitNode $ NCon nilId []
        foldM (\acc byte -> do
            unboxedCharIdx <- emitNode $ NLit (LEChar (fromIntegral byte))
            charIdx <- emitNode $ NCon charId [unboxedCharIdx]
            emitNode $ NCon consId [charIdx, acc]
          ) nilIdx (reverse bytes)

    -- Fallback: unpackCString# with non-static Addr# (e.g., computed via plusAddr#).
    -- Desugar to runtime iteration using IndexCharOffAddr/PlusAddr primops.
    Var v | isUnpackCStringVar v
          , [arg] <- args -> do
        return () -- unpackCString# non-literal fallback
        argIdx <- translate arg
        emitRuntimeUnpackCString argIdx

    -- Fallback: unpackAppendCString# with non-static addr (e.g., show generates these)
    -- Desugar to runtime iteration: go addr suffix where
    --   go a s = case indexCharOffAddr# a 0# of { '\0'# -> s; c -> C# c : go (plusAddr# a 1#) s }
    Var v | isUnpackAppendCStringVar v
          , [litArg, suffixArg] <- args
          , Nothing <- extractAddrLitBytes litArg -> do
        return () -- unpackAppendCString# non-literal fallback
        litIdx <- translate litArg
        suffixIdx <- translate suffixArg
        emitRuntimeUnpackAppendCString litIdx suffixIdx

    -- Partial application of unpackAppendCString# (1 arg only — produces a lambda)
    -- unpackAppendCString# addr → \suffix -> go addr suffix
    Var v | isUnpackAppendCStringVar v
          , [litArg] <- args -> do
        return () -- unpackAppendCString# partial apply
        case extractAddrLitBytes litArg of
          Just bytes -> do
            -- Static: build \suffix -> "prefix" ++ suffix (cons chain ending with suffix)
            sufId <- freshSynthVarId
            sufRef <- emitNode $ NVar sufId
            let consId = varId (dataConWorkId consDataCon)
                charId = varId (dataConWorkId charDataCon)
            recordDC consDataCon
            recordDC charDataCon
            bodyIdx <- foldM (\acc byte -> do
                unboxedCharIdx <- emitNode $ NLit (LEChar (fromIntegral byte))
                charIdx <- emitNode $ NCon charId [unboxedCharIdx]
                emitNode $ NCon consId [charIdx, acc]
              ) sufRef (reverse bytes)
            emitNode $ NLam sufId bodyIdx
          Nothing -> do
            -- Dynamic: build \suffix -> runtime unpackAppend
            litIdx <- translate litArg
            sufId <- freshSynthVarId
            sufRef <- emitNode $ NVar sufId
            bodyIdx <- emitRuntimeUnpackAppendCString litIdx sufRef
            emitNode $ NLam sufId bodyIdx

    -- Zero-arg unpackAppendCString# (eta-reduced): emit as \addr -> \suffix -> go addr suffix
    Var v | isUnpackAppendCStringVar v
          , null args -> do
        return () -- unpackAppendCString# zero-arg (eta-reduced)
        adrId <- freshSynthVarId
        sufId <- freshSynthVarId
        adrRef <- emitNode $ NVar adrId
        sufRef <- emitNode $ NVar sufId
        bodyIdx <- emitRuntimeUnpackAppendCString adrRef sufRef
        lamSuf <- emitNode $ NLam sufId bodyIdx
        emitNode $ NLam adrId lamSuf

    -- Desugar unpackAppendCString# "prefix"# suffix to cons chain:
    -- (:) 'p' ((:) 'r' (... ((:) 'x' suffix)))
    Var v | isUnpackAppendCStringVar v
          , [litArg, suffixArg] <- args
          , Just bytes <- extractAddrLitBytes litArg -> do
        suffixIdx <- translate suffixArg
        let consId = varId (dataConWorkId consDataCon)
            charId = varId (dataConWorkId charDataCon)
        recordDC consDataCon
        recordDC charDataCon
        foldM (\acc byte -> do
            unboxedCharIdx <- emitNode $ NLit (LEChar (fromIntegral byte))
            charIdx <- emitNode $ NCon charId [unboxedCharIdx]
            emitNode $ NCon consId [charIdx, acc]
          ) suffixIdx (reverse bytes)

    -- Desugar unpackFoldrCString# "lit"# f z → f (C# c1) (f (C# c2) (... (f (C# cn) z)))
    -- GHC's build/foldr fusion rewrites foldr/build pairs into unpackFoldrCString#,
    -- whose unfolding uses plusAddr#/indexCharOffAddr# (Addr# pointer arithmetic).
    -- We intercept and expand statically to avoid needing Addr# primops.
    Var v | isUnpackFoldrCStringVar v
          , [litArg, fArg, zArg] <- args
          , Just bytes <- extractAddrLitBytes litArg -> do
        zIdx <- translate zArg
        fIdx <- translate fArg
        let charId = varId (dataConWorkId charDataCon)
        recordDC charDataCon
        foldM (\acc byte -> do
            unboxedCharIdx <- emitNode $ NLit (LEChar (fromIntegral byte))
            charIdx <- emitNode $ NCon charId [unboxedCharIdx]
            fCharIdx <- emitNode $ NApp fIdx charIdx
            emitNode $ NApp fCharIdx acc
          ) zIdx (reverse bytes)

    -- Desugar (++) xs ys → letrec go = \a -> case a of { [] -> ys; (:) x rest -> (:) x (go rest) } in go xs
    -- GHC.Internal.Base.++ has no unfolding available from the .hi file.
    Var v | isAppendVar v, [xsArg, ysArg] <- args -> do
        ysIdx <- translate ysArg
        xsIdx <- translate xsArg
        goId <- freshSynthVarId
        aId <- freshSynthVarId
        xId <- freshSynthVarId
        restId <- freshSynthVarId
        let consId = varId (dataConWorkId consDataCon)
            nilId  = varId (dataConWorkId nilDataCon)
        recordDC consDataCon
        recordDC nilDataCon
        -- Build the cons alt RHS: (:) x (go rest)
        goRef <- emitNode $ NVar goId
        restRef <- emitNode $ NVar restId
        goRestIdx <- emitNode $ NApp goRef restRef
        xRef <- emitNode $ NVar xId
        consResultIdx <- emitNode $ NCon consId [xRef, goRestIdx]
        -- Build: case a of { [] -> ys; (:) x rest -> (:) x (go rest) }
        aRef <- emitNode $ NVar aId
        let nilAlt  = FlatAlt (FDataAlt nilId) [] ysIdx
            consAlt = FlatAlt (FDataAlt consId) [xId, restId] consResultIdx
        caseIdx <- emitNode $ NCase aRef aId [nilAlt, consAlt]
        -- Build: \a -> case ...
        lamIdx <- emitNode $ NLam aId caseIdx
        -- Build: go xs
        goRef2 <- emitNode $ NVar goId
        appIdx <- emitNode $ NApp goRef2 xsIdx
        -- Build: letrec go = \a -> ... in go xs
        emitNode $ NLetRec [(goId, lamIdx)] appIdx

    -- Desugar $wunsafeTake n# xs → recursive list take with unboxed counter.
    -- GHC worker-wrappers `take` at -O2; the worker $wunsafeTake has no unfolding.
    Var v | isUnsafeTakeVar v, [nArg, xsArg] <- args -> do
        nIdx <- translate nArg
        xsIdx <- translate xsArg
        goId <- freshSynthVarId
        iId <- freshSynthVarId
        aId <- freshSynthVarId
        xId <- freshSynthVarId
        restId <- freshSynthVarId
        let consId = varId (dataConWorkId consDataCon)
            nilId  = varId (dataConWorkId nilDataCon)
        recordDC consDataCon
        recordDC nilDataCon
        -- Build: (:) x (go (IntSub i 1) rest)
        goRef1 <- emitNode $ NVar goId
        iRef1 <- emitNode $ NVar iId
        lit1 <- emitNode $ NLit (LEInt 1)
        iSub1 <- emitNode $ NPrimOp (T.pack "IntSub") [iRef1, lit1]
        goISub1 <- emitNode $ NApp goRef1 iSub1
        restRef <- emitNode $ NVar restId
        goISub1Rest <- emitNode $ NApp goISub1 restRef
        xRef <- emitNode $ NVar xId
        consResult <- emitNode $ NCon consId [xRef, goISub1Rest]
        -- Build: case a of { [] -> []; (:) x rest -> (:) x (go (i-1) rest) }
        nilIdx <- emitNode $ NCon nilId []
        aRef <- emitNode $ NVar aId
        let aNilAlt  = FlatAlt (FDataAlt nilId) [] nilIdx
            aConsAlt = FlatAlt (FDataAlt consId) [xId, restId] consResult
        aCaseIdx <- emitNode $ NCase aRef aId [aNilAlt, aConsAlt]
        -- Build: case (IntLe i 0) of { DEFAULT -> <aCaseIdx>; 1# -> [] }
        iRef2 <- emitNode $ NVar iId
        lit0 <- emitNode $ NLit (LEInt 0)
        leResult <- emitNode $ NPrimOp (T.pack "IntLe") [iRef2, lit0]
        let leDefaultAlt = FlatAlt FDefault [] aCaseIdx
            leTrueAlt    = FlatAlt (FLitAlt (LEInt 1)) [] nilIdx
        leCaseIdx <- emitNode $ NCase leResult 0 [leDefaultAlt, leTrueAlt]
        -- Build: \i -> \a -> case (IntLe i 0) of ...
        lamA <- emitNode $ NLam aId leCaseIdx
        lamI <- emitNode $ NLam iId lamA
        -- Build: go n xs
        goRef2 <- emitNode $ NVar goId
        goN <- emitNode $ NApp goRef2 nIdx
        goNXs <- emitNode $ NApp goN xsIdx
        -- Build: letrec go = \i -> \a -> ... in go n xs
        emitNode $ NLetRec [(goId, lamI)] goNXs

    Var v | Just dc <- isDataConWorkId_maybe v
          , length args == valueRepArity dc -> do
        recordDC dc
        childIdxs <- mapM translate args
        emitNode $ NCon (varId v) childIdxs

    -- DataCon wrapper Ids: the wrapper takes *boxed* args (e.g., ByteArray, Int)
    -- but we need *unboxed* args for the worker representation stored in NCon.
    -- Strip single-field box constructors (I#, ByteArray, W#, etc.) from args.
    -- This is needed because GHC unfoldings from .hi files may use the wrapper form
    -- (e.g., Text (ByteArray ba#) (I# off#) (I# len#)) instead of the worker form
    -- (Text ba# off# len#). Without stripping, the NCon fields would be nested Cons
    -- instead of Lits, causing SIGSEGV when the JIT tries to unbox them.
    Var v | Just dc <- isDataConWrapId_maybe v
          , length args == valueRepArity dc -> do
        recordDC dc
        childIdxs <- mapM stripBoxCon args
        emitNode $ NCon (varId (dataConWorkId dc)) childIdxs

    -- unsafeEqualityProof → unit value (always matches the single UnsafeRefl alt)
    -- GHC uses this for GADT equality evidence in freer-simple's Member constraint.
    -- It only appears as a case scrutinee with one alternative, so the tag is irrelevant.
    Var v | isUnsafeEqualityProofVar v -> do
        recordDC unitDataCon
        emitNode $ NCon (varId (dataConWorkId unitDataCon)) []

    -- runRW# @rep @ty f  →  f realWorld#
    -- runRW# applies f to realWorld# (a State# token), which we erase.
    -- After type-arg stripping, args = [f]. Since f is \s -> body, translate body directly.
    Var v | isRunRWVar v -> case args of
      [Lam _ body] -> translate body
      [f] -> do
        -- f is a variable reference: apply it to a dummy unit (state token erased)
        fIdx <- translate f
        dummyIdx <- emitNode $ NLit (LEInt 0)
        emitNode $ NApp fIdx dummyIdx
      [] -> do
        -- Partial application: runRW# with only type args, no value arg yet.
        -- Emit \f -> f 0  (state token erased to dummy 0).
        lamVar <- freshSynthVarId
        fIdx   <- emitNode $ NVar lamVar
        dummyIdx <- emitNode $ NLit (LEInt 0)
        bodyIdx  <- emitNode $ NApp fIdx dummyIdx
        emitNode $ NLam lamVar bodyIdx
      _   -> error $ "runRW# expected 0-1 value args, got " ++ show (length args) ++ ": " ++ showPprUnsafe expr

    -- tagToEnum# @T arg → case arg of { 0# → C0; 1# → C1; ... }
    -- We desugar here because type information is erased downstream.
    Var v | Just pop <- isPrimOpId_maybe v
          , pop == TagToEnumOp
          , length args == 1 -> do
        let typeArgs = filter (not . isValueArg) allArgs
        case typeArgs of
          [Type ty] | Just (tc, _) <- splitTyConApp_maybe ty -> do
            let dcs = tyConDataCons tc
            argIdx <- translate (head args)
            altData <- forM (zip [0..] dcs) $ \(i :: Int, dc) -> do
              recordDC dc
              conIdx <- emitNode $ NCon (varId (dataConWorkId dc)) []
              return $ FlatAlt (FLitAlt (LEInt (fromIntegral i))) [] conIdx
            -- Use VarId 0 as the case binder (unused in alternatives)
            emitNode $ NCase argIdx 0 altData
          _ -> error $ "tagToEnum# without resolvable type argument"

    Var v | Just pop <- isPrimOpId_maybe v
          , length args == primOpArity pop -> do
        childIdxs <- mapM translate args
        emitNode $ NPrimOp (mapPrimOp pop) childIdxs

    Var v | Just arity <- isJoinId_maybe v
          , length allArgs == arity -> do
        recJoins <- gets tsRecJoinIds
        if Set.member (varId v) recJoins
          then do
            -- Rec join point: translated as LetRec lambda, emit App chain
            hIdx <- emitNode $ NVar (varId v)
            childIdxs <- mapM translate args
            foldM (\fIdx aIdx -> emitNode $ NApp fIdx aIdx) hIdx childIdxs
          else do
            childIdxs <- mapM translate args
            emitNode $ NJump (varId v) childIdxs
    
    -- Foreign calls: map known FFI functions to our primops
    Var v | isFCallId v -> do
        let pprName = showPprUnsafe v
        childIdxs <- mapM translate args
        emitNode $ NPrimOp (mapFfiCall pprName) childIdxs

    _ -> do
      hIdx <- translateHead hd
      foldM (\fIdx arg -> do
        aIdx <- translate arg
        emitNode $ NApp fIdx aIdx) hIdx args

translateHead :: CoreExpr -> TransM Int
translateHead = \case
  Var v
    | isRuntimeErrorVar v -> do
        let kind = if occNameString (nameOccName (idName v)) == "divZeroError" then 0 else 1
        emitNode $ NVar (0x4500000000000000 .|. kind)  -- tag 'E' for error
    | isErrorVar v -> emitNode $ NVar 0x4500000000000002  -- tag 'E', kind 2 (error)
    | isUndefinedVar v -> emitNode $ NVar 0x4500000000000003  -- tag 'E', kind 3 (undefined)
    | isRealWorldVar v ->
        emitNode $ NLit (LEInt 0)  -- realWorld# state token → dummy literal
    | isTypeMetadataVar v ->
        emitNode $ NVar 0x4500000000000004  -- tag 'E', kind 4 (type metadata)
    | otherwise -> do
        unresolved <- gets (Set.member (varId v) . tsUnresolvedIds)
        if unresolved
          then emitNode $ NVar 0x4500000000000004
          else emitNode $ NVar (varId v)
  Lit l -> emitNode $ NLit (mapLit l)
  Lam b body
    | isTyVar b -> translate body
    | otherwise -> do
        bodyIdx <- translate body
        emitNode $ NLam (varId b) bodyIdx
  Let (NonRec b rhs) body
    | Just arity <- isJoinId_maybe b
    , jumpCrossesLam (varId b) body -> do
        -- Join point is used inside a lambda in the body — can't compile as
        -- a Cranelift block (lambdas are separate functions). Convert to a
        -- regular LetNonRec with a lambda wrapper, same as Rec join handling.
        let (params, joinBody) = collectValueBinders arity rhs
        joinBodyIdx <- translate joinBody
        rhsIdx <- foldM (\inner p -> emitNode $ NLam (varId p) inner)
                        joinBodyIdx (reverse params)
        modify' $ \s -> s { tsRecJoinIds = Set.insert (varId b) (tsRecJoinIds s) }
        bodyIdx <- translate body
        emitNode $ NLetNonRec (varId b) rhsIdx bodyIdx
    | Just arity <- isJoinId_maybe b -> do
        let (params, joinRhs) = collectValueBinders arity rhs
        joinRhsIdx <- translate joinRhs
        bodyIdx <- translate body
        emitNode $ NJoin (varId b) (map varId params) joinRhsIdx bodyIdx
    | otherwise -> do
        rhsIdx <- translate rhs
        bodyIdx <- translate body
        emitNode $ NLetNonRec (varId b) rhsIdx bodyIdx
  Let (Rec pairs) body -> do
    -- For join point binders in Rec groups (GHC's "joinrec"), strip the
    -- join arity and translate as regular lambdas.  Register them so that
    -- call sites emit NApp chains instead of NJump.
    let recJoins = [varId b | (b, _) <- pairs, isJoinId b]
    modify' $ \s -> s { tsRecJoinIds = tsRecJoinIds s `Set.union` Set.fromList recJoins }
    pairIdxs <- forM pairs $ \(b, rhs) -> do
      rhs' <- case isJoinId_maybe b of
        Just arity -> do
          let (params, joinBody) = collectValueBinders arity rhs
          joinBodyIdx <- translate joinBody
          -- Build nested NLam chain: \p1 -> \p2 -> ... -> joinBody
          foldM (\inner p -> emitNode $ NLam (varId p) inner)
                joinBodyIdx (reverse params)
        Nothing -> translate rhs
      return (varId b, rhs')
    bodyIdx <- translate body
    emitNode $ NLetRec pairIdxs bodyIdx
  -- Desugar multi-return primops: case quotRemInt# a b of (# q, r #) -> body
  -- Split into:
  --   case quotInt# a b of q { DEFAULT ->
  --   case remInt# a b of r { DEFAULT ->
  --   body }}
  -- This ensures both components are forced and a/b are shared.
  Case scrut _caseBinder _ty [Alt (DataAlt _dc) binders body]
    | (Var v, allArgs) <- collectArgs scrut
    , Just pop <- isPrimOpId_maybe v
    , Just (op1Name, op2Name) <- splitMultiReturnPrimOp pop
    , let valArgs = filter isValueArg allArgs
    , [a, b] <- valArgs
    , vBinders <- filter (not . isTyVar) binders
    , [qBinder, rBinder] <- vBinders -> do
        aIdx <- translate a
        bIdx <- translate b
        qValIdx <- emitNode $ NPrimOp op1Name [aIdx, bIdx]
        rValIdx <- emitNode $ NPrimOp op2Name [aIdx, bIdx]
        -- Bind q and r using Case to force them, then translate body
        bodyIdx <- translate body
        -- case rVal of rBinder { DEFAULT -> body }
        rCaseIdx <- emitNode $ NCase rValIdx (varId rBinder) [FlatAlt FDefault [] bodyIdx]
        -- case qVal of qBinder { DEFAULT -> rCaseIdx }
        emitNode $ NCase qValIdx (varId qBinder) [FlatAlt FDefault [] rCaseIdx]
  -- Desugar triple-return primops: case timesInt2# a b of (# hi, lo, ovf #) -> body
  Case scrut _caseBinder _ty [Alt (DataAlt _dc) binders body]
    | (Var v, allArgs) <- collectArgs scrut
    , Just pop <- isPrimOpId_maybe v
    , Just (op1Name, op2Name, op3Name) <- splitTripleReturnPrimOp pop
    , let valArgs = filter isValueArg allArgs
    , [a, b] <- valArgs
    , vBinders <- filter (not . isTyVar) binders
    , [b1, b2, b3] <- vBinders -> do
        aIdx <- translate a
        bIdx <- translate b
        v1Idx <- emitNode $ NPrimOp op1Name [aIdx, bIdx]
        v2Idx <- emitNode $ NPrimOp op2Name [aIdx, bIdx]
        v3Idx <- emitNode $ NPrimOp op3Name [aIdx, bIdx]
        bodyIdx <- translate body
        c3 <- emitNode $ NCase v3Idx (varId b3) [FlatAlt FDefault [] bodyIdx]
        c2 <- emitNode $ NCase v2Idx (varId b2) [FlatAlt FDefault [] c3]
        emitNode $ NCase v1Idx (varId b1) [FlatAlt FDefault [] c2]
  -- Desugar stateful primop/FFI calls returning unboxed tuples with a state token.
  -- Pattern: case op args... s of (# s', results... #) -> body
  -- Where op is a primop or FFI call and the case unpacks an unboxed tuple.
  -- The state token (s and s') is erased, so we:
  --   1. Drop the state token arg from the primop call
  --   2. For 1 result binder: case op args of result { DEFAULT -> body }
  --   3. For 0 result binders (void ops like write): run op, then body
  Case scrut _caseBinder _ty [Alt (DataAlt dc) binders body]
    | isUnboxedTupleDataCon dc
    , (Var v, allArgs) <- collectArgs scrut
    , isPrimOpId_maybe v /= Nothing || isFCallId v
    , let valArgs = filter isValueArg allArgs
    -- Only drop the last value arg if the first result binder has State# type
    -- (stateful primops like readSmallArray#). For pure primops returning unboxed
    -- tuples (like indexSmallArray# :: SmallArray# a -> Int# -> (# a #)), keep all args.
    , vBinders <- filter (not . isTyVar) binders
    , let hasStateBinder = case vBinders of
            (b:_) -> case splitTyConApp_maybe (idType b) of
                       Just (tc, _) -> tc == statePrimTyCon
                       Nothing      -> False
            _ -> False
    , let nonStateArgs = if hasStateBinder
                         then case valArgs of { [] -> []; _ -> init valArgs }
                         else valArgs
    -> do
        childIdxs <- mapM translate nonStateArgs
        -- Emit the primop or FFI call
        opName <- case isPrimOpId_maybe v of
                    Just pop -> return (mapPrimOp pop)
                    Nothing  -> return (mapFfiCall (showPprUnsafe v))
        primIdx <- emitNode $ NPrimOp opName childIdxs
        if hasStateBinder then do
          -- Stateful primop: bind s' (state token) to dummy, bind results to primop
          dummyState <- emitNode $ NLit (LEInt 0)
          case vBinders of
            [s']           -> do
              -- Void op (e.g. writeWord8Array#): force primop for side effects, bind s'
              bodyIdx <- translate body
              inner <- emitNode $ NCase dummyState (varId s') [FlatAlt FDefault [] bodyIdx]
              emitNode $ NCase primIdx (varId s') [FlatAlt FDefault [] inner]
            [s', result]   -> do
              bodyIdx <- translate body
              inner <- emitNode $ NCase dummyState (varId s') [FlatAlt FDefault [] bodyIdx]
              emitNode $ NCase primIdx (varId result) [FlatAlt FDefault [] inner]
            [s', r1, r2]   -> do
              bodyIdx <- translate body
              c2 <- emitNode $ NCase dummyState (varId s') [FlatAlt FDefault [] bodyIdx]
              c1 <- emitNode $ NCase primIdx (varId r2) [FlatAlt FDefault [] c2]
              emitNode $ NCase primIdx (varId r1) [FlatAlt FDefault [] c1]
            [s', r1, r2, r3] -> do
              bodyIdx <- translate body
              c3 <- emitNode $ NCase dummyState (varId s') [FlatAlt FDefault [] bodyIdx]
              c2 <- emitNode $ NCase primIdx (varId r3) [FlatAlt FDefault [] c3]
              c1 <- emitNode $ NCase primIdx (varId r2) [FlatAlt FDefault [] c2]
              emitNode $ NCase primIdx (varId r1) [FlatAlt FDefault [] c1]
            _ -> error $ "Unsupported stateful unboxed tuple arity: " ++ show (length vBinders) ++ " binders"
        else do
          -- Pure primop returning unboxed tuple (e.g. indexSmallArray# -> (# a #))
          -- No state token: bind results directly to primop output
          case vBinders of
            [result] -> do
              bodyIdx <- translate body
              emitNode $ NCase primIdx (varId result) [FlatAlt FDefault [] bodyIdx]
            [r1, r2] -> do
              -- Pure 2-result primop (e.g. decodeDouble_Int64#, casSmallArray#)
              bodyIdx <- translate body
              c1 <- emitNode $ NCase primIdx (varId r2) [FlatAlt FDefault [] bodyIdx]
              emitNode $ NCase primIdx (varId r1) [FlatAlt FDefault [] c1]
            _ -> error $ "Unsupported pure unboxed tuple arity: " ++ show (length vBinders) ++ " binders"
  Case scrut b _alts_ty alts -> do
    scrutIdx <- translate scrut
    altData <- mapM translateAlt alts
    emitNode $ NCase scrutIdx (varId b) altData
  Cast e _ -> translate e
  Tick _ e -> translate e
  Type _ -> error "Bare Type in expression position"
  -- Coercions are zero-cost type evidence (newtype proofs). They appear in
  -- expression position when GHC inlines through vendored code compiled from
  -- source (e.g., newtype Key = Key Text). Emit unit literal as a placeholder.
  Coercion _ -> emitNode $ NLit (LEInt 0)
  App _ _ -> error "App should be handled by translate"
  _ -> error "Unexpected expression form"

translateAlt :: CoreAlt -> TransM FlatAlt
translateAlt (Alt con binders body) = do
  let vBinders = filter (not . isTyVar) binders
  altCon <- mapAltCon con
  bodyIdx <- translate body
  return $ FlatAlt altCon (map varId vBinders) bodyIdx

mapAltCon :: AltCon -> TransM FlatAltCon
mapAltCon = \case
  DataAlt dc -> do
    recordDC dc
    return $ FDataAlt (varId (dataConWorkId dc))
  LitAlt l   -> return $ FLitAlt (mapLit l)
  DEFAULT    -> return FDefault

varId :: Var -> Word64
varId v = case isDataConId_maybe v of
  Just _  -> stableVarId (varName v)
  Nothing -> if isExternalName (varName v)
             then stableVarId (varName v)
             else fromIntegral (getKey (varUnique v))

stableVarId :: Name -> Word64
stableVarId name =
  let modStr = case nameModule_maybe name of
        Just m  -> normalizeMod (moduleNameString (moduleName m))
        Nothing -> "WiredIn"
      normalizeMod s = T.unpack $ T.replace ".Internal" "" (T.pack s)
      occStr = occNameString (nameOccName name)
      fullStr = modStr ++ ":" ++ occStr
      Fingerprint h1 _ = fingerprintString fullStr
      res = (0xFE `shiftL` 56) .|. (h1 .&. 0x00FFFFFFFFFFFFFF)
  in trace ("stableVarId: " ++ fullStr ++ " -> 0x" ++ showHex res "") res

collectValueBinders :: Int -> CoreExpr -> ([Var], CoreExpr)
collectValueBinders 0 e = ([], e)
collectValueBinders n (Lam b e)
  | isTyVar b = collectValueBinders (n-1) e  -- type args count toward join arity
  | otherwise = let (bs, body) = collectValueBinders (n-1) e in (b:bs, body)
-- GHC may eta-reduce join point RHSes; return what we found.
collectValueBinders _ e = ([], e)

isValueArg :: CoreExpr -> Bool
isValueArg (Type _) = False
isValueArg (Coercion _) = False
isValueArg _ = True

-- | Strip a single-field box constructor from a wrapper DataCon arg.
-- When a DataCon wrapper is applied, its args are boxed:
--   Text (ByteArray ba#) (I# off#) (I# len#)
-- We need to strip the boxing to get the worker args:
--   Text ba# off# len#
-- This handles I#, W#, ByteArray, and any other single-field product constructor.
stripBoxCon :: CoreExpr -> TransM Int
stripBoxCon expr =
  let (hd, allArgs) = collectArgs expr
      vArgs = filter isValueArg allArgs
  in case hd of
    Var w | Just innerDc <- isDataConWorkId_maybe w
          , dataConRepArity innerDc == 1
          , [inner] <- vArgs -> do
        recordDC innerDc  -- still record the box constructor for DataConTable
        translate inner
    _ -> translate expr

mapLit :: Literal -> LitEnc
mapLit = \case
  LitNumber nt n  -> case nt of
    LitNumInt    -> LEInt (fromInteger n)
    LitNumInt8   -> LEInt (fromInteger n)
    LitNumInt16  -> LEInt (fromInteger n)
    LitNumInt32  -> LEInt (fromInteger n)
    LitNumInt64  -> LEInt (fromInteger n)
    LitNumWord   -> LEWord (fromInteger n)
    LitNumWord8  -> LEWord (fromInteger n)
    LitNumWord16 -> LEWord (fromInteger n)
    LitNumWord32 -> LEWord (fromInteger n)
    LitNumWord64 -> LEWord (fromInteger n)
    LitNumBigNat -> error "BigNat literal not supported"
  LitChar c              -> LEChar (fromIntegral (ord c))
  LitString bs           -> LEString bs
  LitFloat r             -> LEFloat (fromIntegral (castFloatToWord32 (fromRational r)))
  LitDouble r            -> LEDouble (castDoubleToWord64 (fromRational r))
  LitNullAddr            -> LEInt 0  -- Addr# null → dummy value (dead code path)
  LitLabel{}             -> LEInt 0  -- Function label → dummy value (dead code path)
  LitRubbish{}           -> LEInt 0  -- Rubbish literal → dummy value
  other                  -> error $ "Unsupported literal: " ++ showPprUnsafe other

mapPrimOp :: PrimOp -> Text
mapPrimOp = \case
  IntAddOp    -> "IntAdd"
  IntSubOp    -> "IntSub"
  IntMulOp    -> "IntMul"
  IntNegOp    -> "IntNegate"
  IntEqOp     -> "IntEq"
  IntNeOp     -> "IntNe"
  IntLtOp     -> "IntLt"
  IntLeOp     -> "IntLe"
  IntGtOp     -> "IntGt"
  IntGeOp     -> "IntGe"
  WordAddOp   -> "WordAdd"
  WordSubOp   -> "WordSub"
  WordMulOp   -> "WordMul"
  WordEqOp    -> "WordEq"
  WordNeOp    -> "WordNe"
  WordLtOp    -> "WordLt"
  WordLeOp    -> "WordLe"
  WordGtOp    -> "WordGt"
  WordGeOp    -> "WordGe"
  DoubleAddOp -> "DoubleAdd"
  DoubleSubOp -> "DoubleSub"
  DoubleMulOp -> "DoubleMul"
  DoubleDivOp -> "DoubleDiv"
  DoubleEqOp  -> "DoubleEq"
  DoubleNeOp  -> "DoubleNe"
  DoubleLtOp  -> "DoubleLt"
  DoubleLeOp  -> "DoubleLe"
  DoubleGtOp  -> "DoubleGt"
  DoubleGeOp  -> "DoubleGe"
  CharEqOp    -> "CharEq"
  CharNeOp    -> "CharNe"
  CharLtOp    -> "CharLt"
  CharLeOp    -> "CharLe"
  CharGtOp    -> "CharGt"
  CharGeOp    -> "CharGe"
  IndexArrayOp -> "IndexArray"
  TagToEnumOp -> "TagToEnum"
  DataToTagSmallOp -> "DataToTag"
  DataToTagLargeOp -> "DataToTag"
  IntQuotOp -> "IntQuot"
  IntRemOp  -> "IntRem"
  ChrOp     -> "Chr"
  OrdOp     -> "Ord"
  -- Int bitwise
  IntAndOp  -> "IntAnd"
  IntOrOp   -> "IntOr"
  IntXorOp  -> "IntXor"
  IntNotOp  -> "IntNot"
  IntSllOp  -> "IntShl"
  IntSraOp  -> "IntShra"
  IntSrlOp  -> "IntShrl"
  -- Word arithmetic + bitwise
  WordQuotOp -> "WordQuot"
  WordRemOp  -> "WordRem"
  WordAndOp  -> "WordAnd"
  WordOrOp   -> "WordOr"
  WordXorOp  -> "WordXor"
  WordNotOp  -> "WordNot"
  WordSllOp  -> "WordShl"
  WordSrlOp  -> "WordShrl"
  -- Int↔Word conversions
  IntToWordOp -> "Int2Word"
  WordToIntOp -> "Word2Int"
  -- Narrowing
  Narrow8IntOp   -> "Narrow8Int"
  Narrow16IntOp  -> "Narrow16Int"
  Narrow32IntOp  -> "Narrow32Int"
  Narrow8WordOp  -> "Narrow8Word"
  Narrow16WordOp -> "Narrow16Word"
  Narrow32WordOp -> "Narrow32Word"
  -- Float arithmetic + comparison
  FloatAddOp    -> "FloatAdd"
  FloatSubOp    -> "FloatSub"
  FloatMulOp    -> "FloatMul"
  FloatDivOp    -> "FloatDiv"
  FloatNegOp    -> "FloatNegate"
  FloatEqOp     -> "FloatEq"
  FloatNeOp     -> "FloatNe"
  FloatLtOp     -> "FloatLt"
  FloatLeOp     -> "FloatLe"
  FloatGtOp     -> "FloatGt"
  FloatGeOp     -> "FloatGe"
  -- Double extras
  DoubleNegOp   -> "DoubleNegate"
  -- Type conversions
  IntToDoubleOp   -> "Int2Double"
  DoubleToIntOp   -> "Double2Int"
  IntToFloatOp    -> "Int2Float"
  FloatToIntOp    -> "Float2Int"
  DoubleToFloatOp -> "Double2Float"
  FloatToDoubleOp -> "Float2Double"
  -- Pointer equality (polyfill: always 0# = not equal)
  ReallyUnsafePtrEqualityOp -> "ReallyUnsafePtrEquality"
  -- Addr#
  IndexOffAddrOp_Char -> "IndexCharOffAddr"
  AddrAddOp           -> "PlusAddr"
  -- ByteArray#
  NewByteArrayOp_Char         -> "NewByteArray"
  SizeofByteArrayOp           -> "SizeofByteArray"
  SizeofMutableByteArrayOp    -> "SizeofByteArray"
  UnsafeFreezeByteArrayOp     -> "UnsafeFreezeByteArray"
  CopyAddrToByteArrayOp       -> "CopyAddrToByteArray"
  ReadByteArrayOp_Word8       -> "ReadWord8Array"
  WriteByteArrayOp_Word8      -> "WriteWord8Array"
  IndexByteArrayOp_Word       -> "IndexWordArray"
  WriteByteArrayOp_Word       -> "WriteWordArray"
  ReadByteArrayOp_Word        -> "ReadWordArray"
  SetByteArrayOp              -> "SetByteArray"
  ShrinkMutableByteArrayOp_Char -> "ShrinkMutableByteArray"
  IndexByteArrayOp_Word8      -> "IndexWord8Array"
  IndexOffAddrOp_Word8        -> "IndexWord8OffAddr"
  CopyByteArrayOp             -> "CopyByteArray"
  CopyMutableByteArrayOp      -> "CopyMutableByteArray"
  CompareByteArraysOp         -> "CompareByteArrays"
  GetSizeofMutableByteArrayOp -> "SizeofByteArray"
  ResizeMutableByteArrayOp_Char -> "ResizeMutableByteArray"
  -- Word8
  Word8ToWordOp               -> "Word8ToWord"
  WordToWord8Op               -> "WordToWord8"
  Word8AddOp                  -> "Word8Add"
  Word8SubOp                  -> "Word8Sub"
  Word8LtOp                   -> "Word8Lt"
  Word8LeOp                   -> "Word8Le"
  Word8GeOp                   -> "Word8Ge"
  -- Int64
  Int64AddOp                  -> "Int64Add"
  Int64SubOp                  -> "Int64Sub"
  Int64MulOp                  -> "Int64Mul"
  Int64NegOp                  -> "Int64Negate"
  Int64SllOp                  -> "Int64Shl"
  Int64SraOp                  -> "Int64Shra"
  Int64LtOp                   -> "Int64Lt"
  Int64LeOp                   -> "Int64Le"
  Int64GtOp                   -> "Int64Gt"
  Int64GeOp                   -> "Int64Ge"
  Int64ToIntOp                -> "Int64ToInt"
  IntToInt64Op                -> "IntToInt64"
  Int64ToWord64Op             -> "Int64ToWord64"
  -- Word64
  Word64ToInt64Op             -> "Word64ToInt64"
  Word64SllOp                 -> "Word64Shl"
  Word64OrOp                  -> "Word64Or"
  Word64AndOp                 -> "Word64And"
  -- Carry arithmetic and wide multiply handled by splitMultiReturnPrimOp / splitTripleReturnPrimOp
  -- CLZ
  Clz8Op                      -> "Clz8"
  -- SmallArray#
  NewSmallArrayOp             -> "NewSmallArray"
  ReadSmallArrayOp            -> "ReadSmallArray"
  WriteSmallArrayOp           -> "WriteSmallArray"
  IndexSmallArrayOp           -> "IndexSmallArray"
  SizeofSmallArrayOp          -> "SizeofSmallArray"
  SizeofSmallMutableArrayOp   -> "SizeofSmallMutableArray"
  GetSizeofSmallMutableArrayOp -> "SizeofSmallMutableArray"
  UnsafeFreezeSmallArrayOp    -> "UnsafeFreezeSmallArray"
  UnsafeThawSmallArrayOp      -> "UnsafeThawSmallArray"
  CopySmallArrayOp            -> "CopySmallArray"
  CopySmallMutableArrayOp     -> "CopySmallMutableArray"
  CloneSmallArrayOp           -> "CloneSmallArray"
  CloneSmallMutableArrayOp    -> "CloneSmallMutableArray"
  ShrinkSmallMutableArrayOp_Char -> "ShrinkSmallMutableArray"
  CasSmallArrayOp             -> "CasSmallArray"
  -- Array#
  NewArrayOp                  -> "NewArray"
  ReadArrayOp                 -> "ReadArray"
  WriteArrayOp                -> "WriteArray"
  IndexArrayOp                -> "IndexArray"
  SizeofArrayOp               -> "SizeofArray"
  SizeofMutableArrayOp        -> "SizeofMutableArray"
  UnsafeFreezeArrayOp         -> "UnsafeFreezeArray"
  UnsafeThawArrayOp           -> "UnsafeThawArray"
  CopyArrayOp                 -> "CopyArray"
  CopyMutableArrayOp          -> "CopyMutableArray"
  CloneArrayOp                -> "CloneArray"
  CloneMutableArrayOp         -> "CloneMutableArray"
  -- Bit operations
  PopCntOp                    -> "PopCnt"
  PopCnt8Op                   -> "PopCnt8"
  PopCnt16Op                  -> "PopCnt16"
  PopCnt32Op                  -> "PopCnt32"
  PopCnt64Op                  -> "PopCnt64"
  CtzOp                       -> "Ctz"
  Ctz8Op                      -> "Ctz8"
  Ctz16Op                     -> "Ctz16"
  Ctz32Op                     -> "Ctz32"
  Ctz64Op                     -> "Ctz64"
  -- Exception
  RaiseOp     -> "Raise"
  other       -> trace ("WARNING: unsupported primop: " ++ showPprUnsafe other ++ " (emitting Raise)") "Raise"

collectDataCons :: [TyCon] -> [(Word64, Text, Int, Int, [Text])]
collectDataCons tycons =
  [ (varId (dataConWorkId dc), T.pack (occNameString (nameOccName (dataConName dc))), dataConTag dc, valueRepArity dc, map mapBang (dataConSrcBangs dc))
  | tc <- tycons
  , isAlgTyCon tc
  , dc <- tyConDataCons tc
  ]

mapBang :: HsSrcBang -> Text
mapBang (HsSrcBang _ (HsBang srcUnpack srcBang)) =
  case (srcUnpack, srcBang) of
    (_, SrcStrict) -> "SrcBang"
    (SrcUnpack, _) -> "SrcUnpack"
    _              -> "NoSrcBang"

-- | Wired-in constructors that GHC always knows about but may not appear in
-- mg_tcs or binder types. We include these unconditionally in metadata so
-- that ToCore impls ((), Bool, Char, Int, Word, Double, Float, tuples,
-- Ordering, lists) always find their constructors in the DataConTable.
wiredInDataCons :: [(Word64, Text, Int, Int, [Text])]
wiredInDataCons = concatMap (\dc ->
    [( varId (dataConWorkId dc)
     , T.pack (occNameString (nameOccName (dataConName dc)))
     , dataConTag dc
     , valueRepArity dc
     , map mapBang (dataConSrcBangs dc)
     )]
  ) wiredInList
  where
    wiredInList =
      [ consDataCon, nilDataCon
      , trueDataCon, falseDataCon
      , charDataCon, unitDataCon
      , intDataCon, wordDataCon, doubleDataCon, floatDataCon
      , tupleDataCon Boxed 2  -- (,)
      , tupleDataCon Boxed 3  -- (,,)
      , ordLTDataCon, ordEQDataCon, ordGTDataCon
      ]

-- | Count value arguments excluding GADT equality evidence.
-- dataConRepArity includes equality evidence args (EqSpec) for GADT constructors,
-- but GHC Core passes these as Coercion arguments, which isValueArg filters out.
-- Subtract the EqSpec count to match what the translator sees as "value arguments".
-- For non-GADT constructors (including typeclass dicts), EqSpec is empty so this
-- equals dataConRepArity.
valueRepArity :: DataCon -> Int
valueRepArity dc =
  let (_, _, eqSpec, _, _, _) = dataConFullSig dc
  in dataConRepArity dc - length eqSpec

-- | Recognize GHC's unpackCString# and unpackCStringUtf8# builtins.
-- These convert Addr# (C string pointers) to [Char]. Since our
-- serializer already has the string bytes as LitString, we erase
-- the conversion and keep just the literal.
-- | Recognize GHC.Internal.Base.++ (list append).
isAppendVar :: Id -> Bool
isAppendVar v = occNameString (nameOccName (idName v)) == "++"

isErrorVar :: Id -> Bool
isErrorVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "error" || name == "errorWithoutStackTrace"
     || name == "patError" || name == "noMethodBindingError"
     || name == "recSelError" || name == "recConError"

isUndefinedVar :: Id -> Bool
isUndefinedVar v = occNameString (nameOccName (idName v)) == "undefined"

isUnsafeTakeVar :: Id -> Bool
isUnsafeTakeVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "$wunsafeTake" || name == "unsafeTake"

isRealWorldVar :: Id -> Bool
isRealWorldVar v = occNameString (nameOccName (idName v)) == "realWorld#"

mapFfiCall :: String -> Text
mapFfiCall pprName
  | "strlen" `isInfixOf` pprName                = T.pack "FfiStrlen"
  | "_hs_text_measure_off" `isInfixOf` pprName  = T.pack "FfiTextMeasureOff"
  | "_hs_text_memchr" `isInfixOf` pprName       = T.pack "FfiTextMemchr"
  | "_hs_text_reverse" `isInfixOf` pprName      = T.pack "FfiTextReverse"
  | otherwise = trace ("WARNING: unsupported FFI call: " ++ pprName ++ " (emitting Raise)") $ T.pack "Raise"

isRuntimeErrorVar :: Id -> Bool
isRuntimeErrorVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "divZeroError" || name == "overflowError"

isUnsafeEqualityProofVar :: Id -> Bool
isUnsafeEqualityProofVar v =
  occNameString (nameOccName (idName v)) == "unsafeEqualityProof"

isRunRWVar :: Id -> Bool
isRunRWVar v = occNameString (nameOccName (idName v)) == "runRW#"

-- | Recognize GHC type-representation metadata vars ($tc*, $trModule*, krep$*, $krep*).
-- These have no runtime semantics and no unfoldings; emit as error VarId.
-- These vars can appear deep inside resolved unfoldings (e.g. Typeable infrastructure)
-- and are not reported by resolveExternals as unresolved.
isTypeMetadataVar :: Id -> Bool
isTypeMetadataVar v =
  let name = occNameString (nameOccName (idName v))
  in any (`isPrefixOf` name) ["$trModule", "$krep", "$tc", "krep$", "tr$Module"]

isUnpackCStringVar :: Id -> Bool
isUnpackCStringVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "unpackCString#" || name == "unpackCStringUtf8#"

-- | Recognize GHC's unpackAppendCString# builtin.
-- unpackAppendCString# :: Addr# -> [Char] -> [Char]
-- Prepends a C string literal to a suffix list.
isUnpackAppendCStringVar :: Id -> Bool
isUnpackAppendCStringVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "unpackAppendCString#"

-- | Recognize GHC's unpackFoldrCString# builtin.
-- unpackFoldrCString# :: Addr# -> (Char -> a -> a) -> a -> a
-- GHC's build/foldr fusion rewrites to this; its unfolding uses plusAddr#.
isUnpackFoldrCStringVar :: Id -> Bool
isUnpackFoldrCStringVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "unpackFoldrCString#" || name == "unpackFoldrCStringUtf8#"

-- | Recognize primops that return unboxed tuples and can be split into
-- two individual primops. Returns (primop1, primop2) text names.
splitMultiReturnPrimOp :: PrimOp -> Maybe (Text, Text)
splitMultiReturnPrimOp = \case
  IntQuotRemOp  -> Just (T.pack "IntQuot", T.pack "IntRem")
  WordQuotRemOp -> Just (T.pack "WordQuot", T.pack "WordRem")
  IntAddCOp     -> Just (T.pack "AddIntCVal", T.pack "AddIntCCarry")
  WordAddCOp    -> Just (T.pack "AddWordCVal", T.pack "AddWordCCarry")
  WordSubCOp    -> Just (T.pack "SubWordCVal", T.pack "SubWordCCarry")
  WordMul2Op    -> Just (T.pack "TimesWord2Hi", T.pack "TimesWord2Lo")
  _             -> Nothing

-- | Like splitMultiReturnPrimOp but for primops returning 3-element unboxed tuples.
splitTripleReturnPrimOp :: PrimOp -> Maybe (Text, Text, Text)
splitTripleReturnPrimOp = \case
  IntMul2Op -> Just (T.pack "TimesInt2Hi", T.pack "TimesInt2Lo", T.pack "TimesInt2Overflow")
  _         -> Nothing

-- | Extract Addr# literal bytes from an expression.
-- Handles both direct Lit and Var with an unfolding to Lit
-- (GHC -O2 let-floats Addr# literals into separate bindings).
extractAddrLitBytes :: CoreExpr -> Maybe [Word8]
extractAddrLitBytes (Lit (LitString bs)) = Just (BS.unpack bs)
extractAddrLitBytes (Var v) =
  case maybeUnfoldingTemplate (idUnfolding v) of
    Just (Lit (LitString bs)) -> Just (BS.unpack bs)
    _ -> case maybeUnfoldingTemplate (realIdUnfolding v) of
      Just (Lit (LitString bs)) -> Just (BS.unpack bs)
      _ -> Nothing
extractAddrLitBytes _ = Nothing

primOpArity :: PrimOp -> Int
primOpArity op = let (_, _, _, a, _) = primOpSig op in a

isJoinId_maybe :: Id -> Maybe Int
isJoinId_maybe v = case idJoinPointHood v of
  JoinPoint n -> Just n
  NotJoinPoint -> Nothing

-- | Check if a jump to a given VarId occurs under a Lam in the expression.
-- When this is true, compiling the join point as a Cranelift block won't work
-- because the lambda gets compiled as a separate function with its own context.
jumpCrossesLam :: Word64 -> CoreExpr -> Bool
jumpCrossesLam vid = go False
  where
    go underLam (Var v)   = underLam && varId v == vid
    go underLam (App f a) = go underLam f || go underLam a
    go _        (Lam b e)
      | isTyVar b         = go False e  -- type lambdas don't create new functions
      | otherwise          = go True e
    go underLam (Let b e) = goBind underLam b || go underLam e
    go underLam (Case e _ _ alts) = go underLam e || any (goAlt underLam) alts
    go underLam (Cast e _) = go underLam e
    go underLam (Tick _ e) = go underLam e
    go _ (Lit _)          = False
    go _ (Type _)         = False
    go _ (Coercion _)     = False
    goBind underLam (NonRec _ rhs)  = go underLam rhs
    goBind underLam (Rec pairs)     = any (go underLam . snd) pairs
    goAlt underLam (Alt _ _ e)      = go underLam e
