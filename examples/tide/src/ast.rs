use core_bridge_derive::ToCore;

/// Binary operator tag — mirrors `BinOp` in `haskell/Types.hs`.
#[derive(Debug, Clone, PartialEq, ToCore)]
pub enum BinOp {
    #[core(name = "OpAdd")]    Add,
    #[core(name = "OpSub")]    Sub,
    #[core(name = "OpMul")]    Mul,
    #[core(name = "OpDiv")]    Div,
    #[core(name = "OpEq")]     Eq,
    #[core(name = "OpNe")]     Ne,
    #[core(name = "OpLt")]     Lt,
    #[core(name = "OpGt")]     Gt,
    #[core(name = "OpLe")]     Le,
    #[core(name = "OpGe")]     Ge,
    #[core(name = "OpConcat")] Concat,
}

/// Builtin function tag — mirrors `BuiltinId` in `haskell/Types.hs`.
#[derive(Debug, Clone, PartialEq, ToCore)]
pub enum BuiltinId {
    #[core(name = "BPrint")]     Print,
    #[core(name = "BFetch")]     Fetch,
    #[core(name = "BReadFile")]  ReadFile,
    #[core(name = "BWriteFile")] WriteFile,
    #[core(name = "BLen")]       Len,
    #[core(name = "BStr")]       Str,
    #[core(name = "BInt")]       Int,
    #[core(name = "BConcat")]    Concat,
}

/// Tide expression AST — mirrors `TExpr` in `haskell/Types.hs`.
/// `#[derive(ToCore)]` generates the serialization to GHC Core automatically.
#[derive(Debug, Clone, PartialEq, ToCore)]
pub enum TExpr {
    #[core(name = "TInt")]     TInt(i64),
    #[core(name = "TStr")]     TStr(String),
    #[core(name = "TBool")]    TBool(bool),
    #[core(name = "TVar")]     TVar(String),
    #[core(name = "TList")]    TList(Vec<TExpr>),
    #[core(name = "TApp")]     TApp(Box<TExpr>, Vec<TExpr>),
    #[core(name = "TBuiltin")] TBuiltin(BuiltinId, Vec<TExpr>),
    #[core(name = "TLet")]     TLet(String, Box<TExpr>, Box<TExpr>),
    #[core(name = "TLam")]     TLam(Vec<String>, Box<TExpr>),
    #[core(name = "TIf")]      TIf(Box<TExpr>, Box<TExpr>, Box<TExpr>),
    #[core(name = "TBinOp")]   TBinOp(BinOp, Box<TExpr>, Box<TExpr>),
    #[core(name = "TBind")]    TBind(String, Box<TExpr>),
}
