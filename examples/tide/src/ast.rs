use core_bridge_derive::ToCore;

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
    #[core(name = "TBuiltin")] TBuiltin(i64, Vec<TExpr>),
    #[core(name = "TLet")]     TLet(String, Box<TExpr>, Box<TExpr>),
    #[core(name = "TLam")]     TLam(Vec<String>, Box<TExpr>),
    #[core(name = "TIf")]      TIf(Box<TExpr>, Box<TExpr>, Box<TExpr>),
    #[core(name = "TBinOp")]   TBinOp(i64, Box<TExpr>, Box<TExpr>),
    #[core(name = "TBind")]    TBind(String, Box<TExpr>),
}
