module Tidepool.Translate
  ( translateBinds
  , translateModule
  , translateModuleClosed
  , collectDataCons
  , collectUsedDataCons
  , FlatNode(..)
  , FlatAlt(..)
  , FlatAltCon(..)
  , LitEnc(..)
  , UnresolvedVar(..)
  ) where

import GHC
import GHC.Core
import GHC.Types.Id
import GHC.Types.Var
import GHC.Types.Unique (getKey)
import GHC.Core.DataCon (DataCon, dataConSourceArity, dataConTag, dataConWorkId, dataConName, dataConSrcBangs, HsSrcBang(..), HsBang(..), SrcUnpackedness(..), SrcStrictness(..))
import GHC.Builtin.PrimOps
import GHC.Types.Literal
import GHC.Types.Name (nameOccName, isExternalName, isSystemName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Core.TyCon
import GHC.Core.Type (splitTyConApp_maybe)
import GHC.Types.Basic (JoinPointHood(..))
import GHC.Utils.Outputable (showPprUnsafe)
import GHC.Float (castDoubleToWord64, castFloatToWord32)
import Data.Char (ord)
import Data.Word
import Data.Int
import Data.Text (Text)
import qualified Data.Text as T
import Data.ByteString (ByteString)
import Data.Sequence (Seq, (|>))
import qualified Data.Sequence as Seq
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Control.Monad.State
import Control.Monad (foldM, forM)
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
  }

type TransM = State TransState

emitNode :: FlatNode -> TransM Int
emitNode n = do
  s <- get
  let idx = Seq.length (tsNodes s)
  put s { tsNodes = tsNodes s |> n }
  return idx

recordDC :: DataCon -> TransM ()
recordDC dc = modify' $ \s ->
  s { tsUsedDCs = Map.insert (varId (dataConWorkId dc)) dc (tsUsedDCs s) }

translateBinds :: [CoreBind] -> [(String, Seq FlatNode)]
translateBinds binds = concatMap translateBind binds
  where
    translateBind (NonRec b rhs) =
      let (idx, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty)
          finalNodes = tsNodes s
          rootIdx = Seq.length finalNodes - 1
      in if idx == rootIdx
         then [(occNameString (nameOccName (idName b)), finalNodes)]
         else error "Root index mismatch in NonRec"
    translateBind (Rec pairs) =
      map (\(b, rhs) ->
        let (idx, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty)
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
translateModule :: [CoreBind] -> String -> (Seq FlatNode, Map.Map Word64 DataCon)
translateModule allBinds targetName =
  let targetId = findTargetId targetName allBinds
      (_, finalState) = runState (wrapAllBinds allBinds targetId) (TransState Seq.empty Map.empty Set.empty)
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

    wrapAllBinds :: [CoreBind] -> Id -> TransM Int
    wrapAllBinds [] target = emitNode (NVar (varId target))
    wrapAllBinds (NonRec b rhs : rest) target
      | isTyVar b = wrapAllBinds rest target  -- skip type bindings
      | otherwise = do
          rhsIdx <- translate rhs
          bodyIdx <- wrapAllBinds rest target
          emitNode (NLetNonRec (varId b) rhsIdx bodyIdx)
    wrapAllBinds (Rec pairs : rest) target = do
      let valPairs = filter (not . isTyVar . fst) pairs
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
translateModuleClosed :: [CoreBind] -> String -> (Seq FlatNode, Map.Map Word64 DataCon, [UnresolvedVar])
translateModuleClosed allBinds targetName =
  let (closedBinds, unresolved) = resolveExternals allBinds
      (nodes, usedDCs) = translateModule closedBinds targetName
  in (nodes, usedDCs, unresolved)

-- | Collect all DataCons encountered during translation of Core bindings.
-- This includes constructors from imported packages (e.g. freer-simple's
-- Val, E, Leaf, Node, Union) that aren't in the module's mg_tcs.
collectUsedDataCons :: [CoreBind] -> [(Word64, Text, Int, Int, [Text])]
collectUsedDataCons binds =
  let allDCs = foldMap collectFromBind binds
  in map dcToMeta (Map.elems allDCs)
  where
    collectFromBind (NonRec _ rhs) =
      let (_, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty)
      in tsUsedDCs s
    collectFromBind (Rec pairs) =
      foldMap (\(_, rhs) ->
        let (_, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty)
        in tsUsedDCs s
      ) pairs

    dcToMeta dc =
      ( varId (dataConWorkId dc)
      , T.pack (occNameString (nameOccName (dataConName dc)))
      , dataConTag dc
      , dataConSourceArity dc
      , map mapBang (dataConSrcBangs dc)
      )

translate :: CoreExpr -> TransM Int
translate expr =
  let (hd, allArgs) = collectArgs expr
      args = filter isValueArg allArgs
  in case hd of
    -- Erase unpackCString#/unpackCStringUtf8# applications:
    -- GHC represents string literals as (unpackCString# "addr"#) in Core.
    -- Since our LitString already carries the bytes, just emit the literal.
    Var v | isUnpackCStringVar v
          , [arg] <- args
          , Lit l <- arg -> emitNode $ NLit (mapLit l)

    Var v | Just dc <- isDataConWorkId_maybe v
          , length args == dataConSourceArity dc -> do
        recordDC dc
        childIdxs <- mapM translate args
        emitNode $ NCon (varId v) childIdxs

    -- GADT constructors have separate wrapper Ids that handle type coercions.
    -- isDataConWorkId_maybe returns Nothing for wrappers, so we check separately.
    -- We emit NCon using the *worker* Id since that's what DataConTable indexes.
    Var v | Just dc <- isDataConWrapId_maybe v
          , length args == dataConSourceArity dc -> do
        recordDC dc
        childIdxs <- mapM translate args
        emitNode $ NCon (varId (dataConWorkId dc)) childIdxs

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
          , length args == arity -> do
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
    
    _ -> do
      hIdx <- translateHead hd
      foldM (\fIdx arg -> do
        aIdx <- translate arg
        emitNode $ NApp fIdx aIdx) hIdx args

translateHead :: CoreExpr -> TransM Int
translateHead = \case
  Var v -> emitNode $ NVar (varId v)
  Lit l -> emitNode $ NLit (mapLit l)
  Lam b body
    | isTyVar b -> translate body
    | otherwise -> do
        bodyIdx <- translate body
        emitNode $ NLam (varId b) bodyIdx
  Let (NonRec b rhs) body
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
  Case scrut b _alts_ty alts -> do
    scrutIdx <- translate scrut
    altData <- mapM translateAlt alts
    emitNode $ NCase scrutIdx (varId b) altData
  Cast e _ -> translate e
  Tick _ e -> translate e
  Type _ -> error "Bare Type in expression position"
  Coercion _ -> error "Bare Coercion in expression position"
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
varId v = fromIntegral (getKey (varUnique v))

collectValueBinders :: Int -> CoreExpr -> ([Var], CoreExpr)
collectValueBinders 0 e = ([], e)
collectValueBinders n (Lam b e)
  | isTyVar b = collectValueBinders n e
  | otherwise = let (bs, body) = collectValueBinders (n-1) e in (b:bs, body)
collectValueBinders n e = error $ "collectValueBinders: expected " ++ show n ++ " more value binder(s), but expression has no more lambdas: " ++ showPprUnsafe e

isValueArg :: CoreExpr -> Bool
isValueArg (Type _) = False
isValueArg (Coercion _) = False
isValueArg _ = True

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
  other       -> error $ "Unsupported primop: " ++ showPprUnsafe other

collectDataCons :: [TyCon] -> [(Word64, Text, Int, Int, [Text])]
collectDataCons tycons =
  [ (varId (dataConWorkId dc), T.pack (occNameString (nameOccName (dataConName dc))), dataConTag dc, dataConSourceArity dc, map mapBang (dataConSrcBangs dc))
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

-- | Recognize GHC's unpackCString# and unpackCStringUtf8# builtins.
-- These convert Addr# (C string pointers) to [Char]. Since our
-- serializer already has the string bytes as LitString, we erase
-- the conversion and keep just the literal.
isUnpackCStringVar :: Id -> Bool
isUnpackCStringVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "unpackCString#" || name == "unpackCStringUtf8#"

primOpArity :: PrimOp -> Int
primOpArity op = let (_, _, _, a, _) = primOpSig op in a

isJoinId_maybe :: Id -> Maybe Int
isJoinId_maybe v = case idJoinPointHood v of
  JoinPoint n -> Just n
  NotJoinPoint -> Nothing
