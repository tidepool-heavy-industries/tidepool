{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE TemplateHaskell #-}

-- | Compile-time half of the Suite.hs oracle generator.
--
-- The binding list is not hand-maintained. @SUITE_ORACLE_NAMES@ names a file
-- holding every @expectation_key@ of the projection manifest. Each key is
-- resolved as @Suite.<occurrence>@ in the splice site's scope: GHC's renamer,
-- not a source regex, decides whether the key is a source-declared top.
-- Keys it cannot resolve (simplifier floats such as @t_swap1@, @$w@ workers,
-- @$f@ dictionaries, @$tc@ typeable bindings) are compiler-introduced.
--
-- A resolved top is classified from its reified type into a closed 'Shape'
-- with a monomorphic renderer, or refused as not closed (function or
-- polymorphic type) or unrepresentable (no expectation kind).
module SuiteOracleTH (oracleTable) where

import Control.Exception (evaluate)
import Control.Monad (forM)
import Data.Char (isAlpha, isAlphaNum)
import qualified Data.Text as T
import Language.Haskell.TH
import Language.Haskell.TH.Syntax (addDependentFile)
import System.Environment (lookupEnv)

import SuiteOracleRender

data Shape
  = SInt
  | SBool
  | SChar
  | SDouble
  | SText
  | SList Shape
  | STuple [Shape]
  | SMaybe Shape
  | SEither Shape Shape

data Refusal
  = RefuseNotClosed String
  | RefuseUnrepresentable String

-- | @[(occurrence, OracleEntry)]@ for every manifest expectation key.
oracleTable :: Q Exp
oracleTable = do
  path <- runIO (lookupEnv "SUITE_ORACLE_NAMES") >>= \case
    Just path -> pure path
    Nothing -> fail "SUITE_ORACLE_NAMES must name the manifest expectation-key list"
  addDependentFile path
  contents <- runIO (readFile path)
  let occurrences = filter (not . null) (lines contents)
  entries <- forM occurrences $ \occurrence ->
    [| (occurrence, $(entryFor occurrence)) |]
  listE (map pure entries)

entryFor :: String -> Q Exp
entryFor occurrence
  | not (plainIdentifier occurrence) = [| CompilerIntroduced |]
  | otherwise = lookupValueName ("Suite." ++ occurrence) >>= \case
      Nothing -> [| CompilerIntroduced |]
      Just name -> reify name >>= \case
        VarI _ ty _ -> classifyTop name ty
        DataConI _ ty _ -> classifyTop name ty
        ClassOpI _ ty _ -> refusal (RefuseNotClosed ("class method :: " ++ pprint ty))
        info -> refusal (RefuseUnrepresentable ("unsupported binding " ++ pprint info))

-- | Occurrences carrying @$@, @:@ or other non-identifier characters are
-- never source value names.
plainIdentifier :: String -> Bool
plainIdentifier (c : cs) =
  (isAlpha c || c == '_') && all (\x -> isAlphaNum x || x == '_' || x == '\'') cs
plainIdentifier [] = False

classifyTop :: Name -> Type -> Q Exp
classifyTop name ty = shapeOf True ty >>= \case
  Left refused -> refusal refused
  Right shape ->
    [| SourceValue (() <$ evaluate $(varE name)) ($(renderer shape) $(varE name)) |]

refusal :: Refusal -> Q Exp
refusal (RefuseNotClosed reason) = [| SourceNotClosed reason |]
refusal (RefuseUnrepresentable reason) = [| SourceUnrepresentable reason |]

-- | The first argument is True only for the top's own type: an arrow there
-- means the top is a function, while an arrow in a field means a closed
-- value that contains a function, which has no first-order expectation.
shapeOf :: Bool -> Type -> Q (Either Refusal Shape)
shapeOf top = \case
  ForallT [] [] body -> shapeOf top body
  ForallT {} -> pure (Left (RefuseNotClosed "polymorphic or constrained type"))
  ForallVisT {} -> pure (Left (RefuseNotClosed "visible forall"))
  VarT variable -> pure (Left (RefuseNotClosed ("type variable " ++ pprint variable)))
  ty
    | isFunction ty ->
        pure . Left $
          if top
            then RefuseNotClosed ("function type " ++ pprint ty)
            else RefuseUnrepresentable ("function-valued component " ++ pprint ty)
  AppT ListT element -> fmap SList <$> shapeOf False element
  ty
    | (TupleT arity, components) <- spine ty
    , arity /= 1
    , length components == arity ->
        fmap STuple . sequence <$> mapM (shapeOf False) components
  AppT (ConT constructor) element
    | constructor == ''Maybe -> fmap SMaybe <$> shapeOf False element
  AppT (AppT (ConT constructor) left) right
    | constructor == ''Either -> do
        left' <- shapeOf False left
        right' <- shapeOf False right
        pure (SEither <$> left' <*> right')
  ConT constructor
    | constructor == ''Int -> ok SInt
    | constructor == ''Bool -> ok SBool
    | constructor == ''Char -> ok SChar
    | constructor == ''Double -> ok SDouble
    | constructor == ''T.Text -> ok SText
    | constructor == tupleTypeName 0 -> ok (STuple [])
    | otherwise -> reify constructor >>= \case
        -- Synonyms such as String and Suite's own `type Text = T.Text`.
        TyConI (TySynD _ [] rhs) -> shapeOf top rhs
        _ -> pure (Left (RefuseUnrepresentable ("type " ++ show constructor ++ " has no expectation kind")))
  ty -> pure (Left (RefuseUnrepresentable ("type " ++ pprint ty ++ " has no expectation kind")))
  where
    ok = pure . Right

isFunction :: Type -> Bool
isFunction = \case
  AppT (AppT ArrowT _) _ -> True
  AppT (AppT (AppT MulArrowT _) _) _ -> True
  _ -> False

spine :: Type -> (Type, [Type])
spine = go []
  where
    go arguments (AppT function argument) = go (argument : arguments) function
    go arguments headType = (headType, arguments)

-- | A monomorphic renderer for one closed shape.
renderer :: Shape -> Q Exp
renderer = \case
  SInt -> [| renderInt |]
  SBool -> [| renderBool |]
  SChar -> [| renderChar |]
  SDouble -> [| renderDouble |]
  SText -> [| renderText |]
  SList element -> [| renderList $(renderer element) |]
  SMaybe element -> [| renderMaybe $(renderer element) |]
  SEither left right -> [| renderEither $(renderer left) $(renderer right) |]
  STuple components -> do
    names <- mapM (const (newName "component")) components
    lamE
      [tupP (map varP names)]
      [| renderTuple $(listE [ [| $(renderer shape) $(varE name) |] | (shape, name) <- zip components names ]) |]
