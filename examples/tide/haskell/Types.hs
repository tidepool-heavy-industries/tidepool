module Types where

data BinOp = OpAdd | OpSub | OpMul | OpDiv
           | OpEq | OpNe | OpLt | OpGt | OpLe | OpGe
           | OpConcat

data BuiltinId = BPrint | BFetch | BReadFile | BWriteFile
               | BLen | BStr | BInt | BConcat

data TExpr
  = TInt Int
  | TStr String
  | TBool Bool
  | TVar String
  | TList [TExpr]
  | TApp TExpr [TExpr]
  | TBuiltin BuiltinId [TExpr]
  | TLet String TExpr TExpr
  | TLam [String] TExpr
  | TIf TExpr TExpr TExpr
  | TBinOp BinOp TExpr TExpr
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
