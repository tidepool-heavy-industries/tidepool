{-# LANGUAGE OverloadedStrings #-}
module Types where

import Data.Text (Text)

data BinOp = OpAdd | OpSub | OpMul | OpDiv
           | OpEq | OpNe | OpLt | OpGt | OpLe | OpGe
           | OpConcat

data BuiltinId = BPrint | BFetch | BReadFile | BWriteFile
               | BLen | BStr | BInt | BConcat

data TExpr
  = TInt Int
  | TStr Text
  | TBool Bool
  | TVar Text
  | TList [TExpr]
  | TApp TExpr [TExpr]
  | TBuiltin BuiltinId [TExpr]
  | TLet Text TExpr TExpr
  | TLam [Text] TExpr
  | TIf TExpr TExpr TExpr
  | TBinOp BinOp TExpr TExpr
  | TBind Text TExpr

data EvalError
  = TypeError Text
  | UndefinedVar Text
  | NotAFunction
  | ArityError Text

data TVal
  = VInt Int
  | VStr Text
  | VBool Bool
  | VList [TVal]
  | VUnit
  | VFun [Text] TExpr [(Text, TVal)]
  | VError EvalError
