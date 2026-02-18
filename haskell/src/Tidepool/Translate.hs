module Tidepool.Translate
  ( translateBinds
  , translateModule
  , collectDataCons
  , collectUsedDataCons
  , FlatNode(..)
  , FlatAlt(..)
  , FlatAltCon(..)
  , LitEnc(..)
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
import Control.Monad.State
import Control.Monad (foldM, forM)

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
      let (idx, s) = runState (translate rhs) (TransState Seq.empty Map.empty)
          finalNodes = tsNodes s
          rootIdx = Seq.length finalNodes - 1
      in if idx == rootIdx
         then [(occNameString (nameOccName (idName b)), finalNodes)]
         else error "Root index mismatch in NonRec"
    translateBind (Rec pairs) =
      map (\(b, rhs) ->
        let (idx, s) = runState (translate rhs) (TransState Seq.empty Map.empty)
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
      (_, finalState) = runState (wrapAllBinds allBinds targetId) (TransState Seq.empty Map.empty)
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
          pairIdxs <- forM valPairs $ \(b, rhs) -> do
            rhsIdx <- translate rhs
            return (varId b, rhsIdx)
          bodyIdx <- wrapAllBinds rest target
          emitNode (NLetRec pairIdxs bodyIdx)

-- | Collect all DataCons encountered during translation of Core bindings.
-- This includes constructors from imported packages (e.g. freer-simple's
-- Val, E, Leaf, Node, Union) that aren't in the module's mg_tcs.
collectUsedDataCons :: [CoreBind] -> [(Word64, Text, Int, Int, [Text])]
collectUsedDataCons binds =
  let allDCs = foldMap collectFromBind binds
  in map dcToMeta (Map.elems allDCs)
  where
    collectFromBind (NonRec _ rhs) =
      let (_, s) = runState (translate rhs) (TransState Seq.empty Map.empty)
      in tsUsedDCs s
    collectFromBind (Rec pairs) =
      foldMap (\(_, rhs) ->
        let (_, s) = runState (translate rhs) (TransState Seq.empty Map.empty)
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
    Var v | Just dc <- isDataConWorkId_maybe v
          , length args == dataConSourceArity dc -> do
        recordDC dc
        childIdxs <- mapM translate args
        emitNode $ NCon (varId v) childIdxs
    
    Var v | Just pop <- isPrimOpId_maybe v
          , length args == primOpArity pop -> do
        childIdxs <- mapM translate args
        emitNode $ NPrimOp (mapPrimOp pop) childIdxs

    Var v | Just arity <- isJoinId_maybe v
          , length args == arity -> do
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
    -- We need to be careful with LetRec if it contains join points.
    -- The spec says: "If a Rec binding group contains join points (isJoinId_maybe), error with a clear message."
    if any (isJoinId . fst) pairs
      then error "LetRec contains join points, which is not supported in v1."
      else do
        pairIdxs <- forM pairs $ \(b, rhs) -> do
          rhsIdx <- translate rhs
          return (varId b, rhsIdx)
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

primOpArity :: PrimOp -> Int
primOpArity op = let (_, _, _, a, _) = primOpSig op in a

isJoinId_maybe :: Id -> Maybe Int
isJoinId_maybe v = case idJoinPointHood v of
  JoinPoint n -> Just n
  NotJoinPoint -> Nothing
