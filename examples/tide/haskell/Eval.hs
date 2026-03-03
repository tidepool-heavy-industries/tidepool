{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
module Eval (eval, repl, showVal, showInt, showError) where

import Data.Text (Text)
import qualified Data.Text as T
import Types
import Effects

type TideEffs = '[Repl, Console, Env, Net, Fs]

-- | Convert Int to Text without using Prelude's show.
-- Uses quot/rem separately (NOT quotRem which returns unboxed tuples).
showInt :: Int -> Text
showInt n
  | n < (0 :: Int)  = T.pack ('-' : showPos (negate n))
  | n == (0 :: Int)  = "0"
  | otherwise        = T.pack (showPos n)

showPos :: Int -> String
showPos n
  | n == (0 :: Int) = ""
  | otherwise       = showPos (quot n (10 :: Int)) ++ [digitToChar (rem n (10 :: Int))]

digitToChar :: Int -> Char
digitToChar d = case d of
  0 -> '0'
  1 -> '1'
  2 -> '2'
  3 -> '3'
  4 -> '4'
  5 -> '5'
  6 -> '6'
  7 -> '7'
  8 -> '8'
  9 -> '9'
  _ -> '?'

-- | Show an evaluation error.
showError :: EvalError -> Text
showError e = case e of
  TypeError s    -> T.append "Type error: " s
  UndefinedVar x -> T.append "Undefined: " x
  NotAFunction   -> "not a function"
  ArityError s   -> s

-- | Convert TVal to display string.
showVal :: TVal -> Text
showVal v = case v of
  VInt n     -> showInt n
  VStr s     -> s
  VBool b    -> if b then "true" else "false"
  VUnit      -> "()"
  VFun _ _ _ -> "<function>"
  VList vs   -> T.append "[" (T.append (showListVals vs) "]")
  VError e   -> T.append "<error: " (T.append (showError e) ">")

showListVals :: [TVal] -> Text
showListVals xs = case xs of
  []     -> ""
  (v:[]) -> showVal v
  (v:vs) -> T.append (showVal v) (T.append ", " (showListVals vs))

-- | Custom list length (avoids Foldable typeclass).
listLength :: [a] -> Int
listLength xs = case xs of
  []     -> (0 :: Int)
  (_:rest) -> (1 :: Int) + listLength rest

-- | Evaluate a Tide expression.
eval :: TExpr -> Eff TideEffs TVal
eval expr = case expr of
  TInt n  -> pure (VInt n)
  TStr s  -> pure (VStr s)
  TBool b -> pure (VBool b)
  TVar x  -> do
    mval <- envLookup x
    case mval of
      Just v  -> pure v
      Nothing -> pure (VError (UndefinedVar x))
  TList es -> do
    vs <- evalList es
    pure (VList vs)
  TBinOp op l r -> do
    lv <- eval l
    rv <- eval r
    evalBinOp op lv rv
  TBuiltin bid args -> do
    vs <- evalList args
    evalBuiltin bid vs
  TLet name e body -> do
    old <- envLookup name
    v <- eval e
    envExtend name v
    result <- eval body
    case old of
      Just prev -> envExtend name prev
      Nothing   -> envRemove name
    pure result
  TBind name e -> do
    v <- eval e
    envExtend name v
    pure v
  TLam params body -> do
    caps <- envSnapshot
    pure (VFun params body caps)
  TApp f args -> do
    fv <- eval f
    vs <- evalList args
    applyFun fv vs
  TIf cond t e -> do
    cv <- eval cond
    case cv of
      VBool True  -> eval t
      VBool False -> eval e
      _           -> pure (VError (TypeError "non-boolean condition"))

-- | Evaluate a list of expressions (explicit recursion, no map/traverse).
evalList :: [TExpr] -> Eff TideEffs [TVal]
evalList xs = case xs of
  []     -> pure []
  (e:es) -> do
    v  <- eval e
    vs <- evalList es
    pure (v : vs)

-- | Evaluate binary operator.
evalBinOp :: BinOp -> TVal -> TVal -> Eff TideEffs TVal
evalBinOp op lv rv = case op of
  OpAdd    -> intOp lv rv (\a b -> VInt (a + b))
  OpSub    -> intOp lv rv (\a b -> VInt (a - b))
  OpMul    -> intOp lv rv (\a b -> VInt (a * b))
  OpDiv    -> intOp lv rv (\a b -> VInt (quot a b))
  OpEq     -> pure (VBool (valEq lv rv))
  OpNe     -> pure (VBool (not (valEq lv rv)))
  OpLt     -> intOp lv rv (\a b -> VBool (a < b))
  OpGt     -> intOp lv rv (\a b -> VBool (a > b))
  OpLe     -> intOp lv rv (\a b -> VBool (a <= b))
  OpGe     -> intOp lv rv (\a b -> VBool (a >= b))
  OpConcat -> case lv of
    VStr a -> case rv of
      VStr b -> pure (VStr (T.append a b))
      _      -> pure (VError (TypeError "++ expects strings"))
    _      -> pure (VError (TypeError "++ expects strings"))

-- | Apply integer binary operation.
intOp :: TVal -> TVal -> (Int -> Int -> TVal) -> Eff TideEffs TVal
intOp (VInt a) (VInt b) f = pure (f a b)
intOp _ _ _ = pure (VError (TypeError "expected integers"))

-- | Value equality (structural).
valEq :: TVal -> TVal -> Bool
valEq (VInt a) (VInt b) = a == b
valEq (VStr a) (VStr b) = a == b
valEq (VBool a) (VBool b) = if a then b else not b
valEq VUnit VUnit = True
valEq _ _ = False

-- | Apply function value to arguments.
applyFun :: TVal -> [TVal] -> Eff TideEffs TVal
applyFun fv args = case fv of
  VFun params body caps -> do
    -- Collect all names we'll modify (params + captured vars)
    let capNames = mapFst caps
    let allNames = params ++ capNames
    olds <- saveParams allNames
    -- Restore captured environment, then bind params on top
    restoreCaps caps
    bindParams params args
    result <- eval body
    restoreParams allNames olds
    pure result
  _ -> pure (VError NotAFunction)

-- | Save current bindings for parameter names.
saveParams :: [Text] -> Eff TideEffs [Maybe TVal]
saveParams ps = case ps of
  []     -> pure []
  (p:rest) -> do
    old <- envLookup p
    olds <- saveParams rest
    pure (old : olds)

-- | Restore saved bindings for parameter names.
restoreParams :: [Text] -> [Maybe TVal] -> Eff TideEffs ()
restoreParams ps olds = case (ps, olds) of
  ([], _) -> pure ()
  (_, []) -> pure ()
  (p:prest, o:orest) -> do
    case o of
      Just prev -> envExtend p prev
      Nothing   -> envRemove p
    restoreParams prest orest

-- | Bind parameters in environment.
bindParams :: [Text] -> [TVal] -> Eff TideEffs ()
bindParams ps as' = case ps of
  [] -> pure ()
  (p:rest) -> case as' of
    []     -> pure ()
    (a:restA) -> do
      envExtend p a
      bindParams rest restA

-- | Dispatch builtin by ID.
evalBuiltin :: BuiltinId -> [TVal] -> Eff TideEffs TVal
evalBuiltin bid args = case bid of
  BPrint -> case args of
    (v:[]) -> do
      printLine (showVal v)
      pure VUnit
    _ -> pure (VError (ArityError "print: 1 arg expected"))
  BFetch -> case args of
    (VStr url : []) -> do
      s <- httpGet url
      pure (VStr s)
    _ -> pure (VError (ArityError "fetch: string arg expected"))
  BReadFile -> case args of
    (VStr path : []) -> do
      s <- fsRead path
      pure (VStr s)
    _ -> pure (VError (ArityError "read_file: string arg expected"))
  BWriteFile -> case args of
    (VStr path : VStr contents : []) -> do
      fsWrite path contents
      pure VUnit
    _ -> pure (VError (ArityError "write_file: 2 string args expected"))
  BLen -> case args of
    (VList vs : []) -> pure (VInt (listLength vs))
    (VStr s : [])   -> pure (VInt (T.length s))
    _ -> pure (VError (ArityError "len: list or string expected"))
  BStr -> case args of
    (v:[]) -> pure (VStr (showVal v))
    _ -> pure (VError (ArityError "str: 1 arg expected"))
  BInt -> case args of
    (VInt n : []) -> pure (VInt n)
    _ -> pure (VError (ArityError "int: numeric arg expected"))
  BConcat -> case args of
    (VStr a : VStr b : []) -> pure (VStr (T.append a b))
    _ -> pure (VError (ArityError "concat: 2 string args expected"))

-- | Extract first elements from a list of pairs.
mapFst :: [(a, b)] -> [a]
mapFst xs = case xs of
  []          -> []
  ((a, _):rest) -> a : mapFst rest

-- | Restore captured variable bindings into the environment.
restoreCaps :: Member Env effs => [(Text, TVal)] -> Eff effs ()
restoreCaps caps = case caps of
  []            -> pure ()
  ((k, v):rest) -> do
    envExtend k v
    restoreCaps rest

-- | Main REPL loop.
repl :: Eff TideEffs ()
repl = do
  mExpr <- readLine'
  case mExpr of
    Nothing -> pure ()
    Just expr -> do
      val <- eval expr
      case val of
        VUnit    -> pure ()
        VError e -> display (T.append "Error: " (showError e))
        _        -> display (showVal val)
      repl
