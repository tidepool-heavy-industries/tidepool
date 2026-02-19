{-# LANGUAGE DataKinds, TypeOperators #-}
module Eval (eval, repl, showVal, showInt, showError) where

import Types
import Effects

type TideEffs = '[Repl, Console, Env, Net, Fs]

-- | Convert Int to String without using Prelude's show.
-- Uses quot/rem separately (NOT quotRem which returns unboxed tuples).
showInt :: Int -> String
showInt n
  | n < (0 :: Int)  = '-' : showPos (negate n)
  | n == (0 :: Int)  = "0"
  | otherwise        = showPos n

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
showError :: EvalError -> String
showError e = case e of
  TypeError s    -> "Type error: " ++ s
  UndefinedVar x -> "Undefined: " ++ x
  NotAFunction   -> "not a function"
  ArityError s   -> s

-- | Convert TVal to display string.
showVal :: TVal -> String
showVal v = case v of
  VInt n     -> showInt n
  VStr s     -> s
  VBool b    -> if b then "true" else "false"
  VUnit      -> "()"
  VFun _ _   -> "<function>"
  VList vs   -> "[" ++ showListVals vs ++ "]"
  VError e   -> "<error: " ++ showError e ++ ">"

showListVals :: [TVal] -> String
showListVals xs = case xs of
  []     -> ""
  (v:[]) -> showVal v
  (v:vs) -> showVal v ++ ", " ++ showListVals vs

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
  TBinOp opId l r -> do
    lv <- eval l
    rv <- eval r
    evalBinOp opId lv rv
  TBuiltin bId args -> do
    vs <- evalList args
    evalBuiltin bId vs
  TLet name e body -> do
    old <- envLookup name
    v <- eval e
    envExtend name v
    result <- eval body
    case old of
      Just prev -> envExtend name prev
      Nothing   -> pure ()
    pure result
  TLam params body ->
    pure (VFun params body)
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
evalBinOp :: Int -> TVal -> TVal -> Eff TideEffs TVal
evalBinOp opId lv rv = case opId of
  0 -> intOp lv rv (\a b -> VInt (a + b))
  1 -> intOp lv rv (\a b -> VInt (a - b))
  2 -> intOp lv rv (\a b -> VInt (a * b))
  3 -> intOp lv rv (\a b -> VInt (quot a b))
  4 -> pure (VBool (valEq lv rv))
  5 -> pure (VBool (not (valEq lv rv)))
  6 -> intOp lv rv (\a b -> VBool (a < b))
  7 -> intOp lv rv (\a b -> VBool (a > b))
  8 -> intOp lv rv (\a b -> VBool (a <= b))
  9 -> intOp lv rv (\a b -> VBool (a >= b))
  10 -> case lv of
    VStr a -> case rv of
      VStr b -> pure (VStr (a ++ b))
      _      -> pure (VError (TypeError "++ expects strings"))
    _      -> pure (VError (TypeError "++ expects strings"))
  _ -> pure (VError (TypeError ("unknown operator: " ++ showInt opId)))

-- | Apply integer binary operation.
intOp :: TVal -> TVal -> (Int -> Int -> TVal) -> Eff TideEffs TVal
intOp (VInt a) (VInt b) f = pure (f a b)
intOp _ _ _ = pure (VError (TypeError "expected integers"))

-- | Value equality (structural).
valEq :: TVal -> TVal -> Bool
valEq (VInt a) (VInt b) = a == b
valEq (VStr a) (VStr b) = strEq a b
valEq (VBool a) (VBool b) = if a then b else not b
valEq VUnit VUnit = True
valEq _ _ = False

-- | String equality (explicit recursion, avoids Eq [Char] typeclass).
strEq :: String -> String -> Bool
strEq xs ys = case xs of
  [] -> case ys of
    [] -> True
    _  -> False
  (a:as') -> case ys of
    []      -> False
    (b:bs) -> if a == b then strEq as' bs else False

-- | Apply function value to arguments.
applyFun :: TVal -> [TVal] -> Eff TideEffs TVal
applyFun fv args = case fv of
  VFun params body -> do
    bindParams params args
    eval body
  _ -> pure (VError NotAFunction)

-- | Bind parameters in environment.
bindParams :: [String] -> [TVal] -> Eff TideEffs ()
bindParams ps as' = case ps of
  [] -> pure ()
  (p:rest) -> case as' of
    []     -> pure ()
    (a:restA) -> do
      envExtend p a
      bindParams rest restA

-- | Dispatch builtin by ID.
evalBuiltin :: Int -> [TVal] -> Eff TideEffs TVal
evalBuiltin bId args = case bId of
  0 -> case args of
    (v:[]) -> do
      printLine (showVal v)
      pure VUnit
    _ -> pure (VError (ArityError "print: 1 arg expected"))
  1 -> case args of
    (VStr url : []) -> do
      s <- httpGet url
      pure (VStr s)
    _ -> pure (VError (ArityError "fetch: string arg expected"))
  2 -> case args of
    (VStr path : []) -> do
      s <- fsRead path
      pure (VStr s)
    _ -> pure (VError (ArityError "read_file: string arg expected"))
  3 -> case args of
    (VStr path : VStr contents : []) -> do
      fsWrite path contents
      pure VUnit
    _ -> pure (VError (ArityError "write_file: 2 string args expected"))
  4 -> case args of
    (VList vs : []) -> pure (VInt (listLength vs))
    (VStr s : [])   -> pure (VInt (listLength s))
    _ -> pure (VError (ArityError "len: list or string expected"))
  5 -> case args of
    (v:[]) -> pure (VStr (showVal v))
    _ -> pure (VError (ArityError "str: 1 arg expected"))
  6 -> case args of
    (VInt n : []) -> pure (VInt n)
    _ -> pure (VError (ArityError "int: numeric arg expected"))
  7 -> case args of
    (VStr a : VStr b : []) -> pure (VStr (a ++ b))
    _ -> pure (VError (ArityError "concat: 2 string args expected"))
  _ -> pure (VError (TypeError ("unknown builtin: " ++ showInt bId)))

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
        VError e -> display ("Error: " ++ showError e)
        _        -> display (showVal val)
      repl
