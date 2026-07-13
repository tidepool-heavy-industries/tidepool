//! Effect-declaration layer for the Tidepool MCP server.
//!
//! Defines [`EffectDecl`] (static Haskell-side metadata for an effect type),
//! the [`DescribeEffect`] / [`CollectEffectDecls`] traits used to gather
//! declarations from an HList of handlers, and the standard `*_decl()`
//! builders, one per effect type. These mostly assemble Haskell-source strings
//! consumed by the preamble/tool-description assembly.

// ---------------------------------------------------------------------------
// Effect metadata — lives next to the handler, discovered via trait
// ---------------------------------------------------------------------------

/// Static metadata describing a Haskell effect type.
///
/// Each effect handler that wants to participate in the MCP templating system
/// implements `DescribeEffect` to provide its Haskell-side type declaration.
#[derive(Debug, Clone, Copy)]
pub struct EffectDecl {
    /// Haskell GADT type name, e.g. `"Console"`.
    pub type_name: &'static str,
    /// Human-readable description of what this effect does.
    pub description: &'static str,
    /// Haskell GADT constructor declarations (one per line inside `data T a where`).
    pub constructors: &'static [&'static str],
    /// Extra Haskell type/function definitions emitted before the GADT.
    /// Use for supporting types (e.g. `data Lang = ...`) and helper functions.
    pub type_defs: &'static [&'static str],
    /// Thin curried helper definitions emitted after the `type M` alias.
    /// Each string is one or more lines of Haskell (signature + definition).
    pub helpers: &'static [&'static str],
}

/// Parsed constructor info extracted from an EffectDecl constructor string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedConstructor {
    pub name: String,
    pub arity: u32,
}

/// Parse `"GitLog :: Text -> Int -> Git [Value]"` → `ParsedConstructor { name: "GitLog", arity: 2 }`
///
/// Arity = number of `->` in the type signature (each `->` separates one argument from the rest).
pub fn parse_constructor(decl: &str) -> Result<ParsedConstructor, String> {
    let (name_part, type_part) = decl
        .split_once("::")
        .ok_or_else(|| format!("constructor decl must contain '::': {:?}", decl))?;
    let name = name_part.trim().to_string();
    let arity = type_part.matches("->").count() as u32;
    Ok(ParsedConstructor { name, arity })
}

/// Trait for effect handlers that can describe their Haskell-side type.
pub trait DescribeEffect {
    fn effect_decl() -> EffectDecl;
}

/// Trait for collecting effect declarations from an HList of handlers.
pub trait CollectEffectDecls {
    fn collect_decls() -> Vec<EffectDecl>;
}

impl CollectEffectDecls for frunk::HNil {
    fn collect_decls() -> Vec<EffectDecl> {
        Vec::new()
    }
}

impl<H, T> CollectEffectDecls for frunk::HCons<H, T>
where
    H: DescribeEffect,
    T: CollectEffectDecls,
{
    fn collect_decls() -> Vec<EffectDecl> {
        let mut decls = vec![H::effect_decl()];
        decls.extend(T::collect_decls());
        decls
    }
}

// ---------------------------------------------------------------------------
// Standard effect declarations
// ---------------------------------------------------------------------------

// Console effect: `console_decl()` is generated from the single-source
// definition in `effect_defs.rs` (the T6 spike prototype) — constructors,
// description, and helper docstrings all live THERE, alongside the facts the
// Rust half (`ConsoleReq`, dispatch) projects from the same table.
crate::console_effect_def!(crate::effect_defs::effect_decl_projection);

// KV effect: `kv_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::kv_effect_def!(crate::effect_defs::effect_decl_projection);

// Fs effect: `fs_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::fs_effect_def!(crate::effect_defs::effect_decl_projection);

// Lsp effect: `lsp_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::lsp_effect_def!(crate::effect_defs::effect_decl_projection);

// Http effect: `http_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::http_effect_def!(crate::effect_defs::effect_decl_projection);

// Exec effect: `exec_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::exec_effect_def!(crate::effect_defs::effect_decl_projection);

// Git effect: `git_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::git_effect_def!(crate::effect_defs::effect_decl_projection);

// Time effect: `time_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::time_effect_def!(crate::effect_defs::effect_decl_projection);

// Meta effect: `meta_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::meta_effect_def!(crate::effect_defs::effect_decl_projection);

// Ask effect: `ask_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::ask_effect_def!(crate::effect_defs::effect_decl_projection);

// Llm effect: `llm_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::llm_effect_def!(crate::effect_defs::effect_decl_projection);
