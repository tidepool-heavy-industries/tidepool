module Types where

data TExpr
  = TInt Int
  | TStr String
  | TBool Bool
  | TVar String
  | TList [TExpr]
  | TApp TExpr [TExpr]
  | TBuiltin Int [TExpr]
  | TLet String TExpr TExpr
  | TLam [String] TExpr
  | TIf TExpr TExpr TExpr
  | TBinOp Int TExpr TExpr
  | TBind String TExpr

data EvalError
  = TypeError String
  | UndefinedVar String
  | NotAFunction
  | ArityError String

data TVal
  = VInt Int
  | VStr String
  | VBool Bool
  | VList [TVal]
  | VUnit
  | VFun [String] TExpr
  | VError EvalError
