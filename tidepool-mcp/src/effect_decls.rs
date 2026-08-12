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
    /// Human-readable description of what this effect does — the long-form
    /// text the eval tool description is assembled from.
    pub description: &'static str,
    /// A COMPACT per-turn variant of `description` — signatures plus one or
    /// two examples, not the full eval-tool essay — for callers that fold
    /// over a decl list to build a verb cheatsheet re-sent every round/turn
    /// (a system-prompt "Available effects" section). `None` falls back to
    /// `description` at the fold site: most effects don't need a distinct
    /// compact form, only ones whose `description` carries multi-line
    /// worked examples (e.g. `AskUser`) set this. This is the ONE place a
    /// per-effect card's text lives — never a second hand-authored table at
    /// a call site.
    pub prompt_card: Option<&'static str>,
    /// Haskell GADT constructor declarations (one per line inside `data T a where`).
    pub constructors: &'static [&'static str],
    /// Extra Haskell type/function definitions emitted before the GADT.
    /// Use for supporting types (e.g. `data Lang = ...`) and helper functions.
    pub type_defs: &'static [&'static str],
    /// Extra `import` lines this effect's helpers need beyond the fixed eval
    /// surface (`eval_import_lines`) — e.g. `Exec` needs `Tidepool.Shell`/
    /// `Tidepool.Cargo` (its helpers build on `runArgv`), `Git` needs
    /// `Tidepool.Git`, `AskUser` needs `Tidepool.Form` (built on
    /// `askUserRaw`). Emitted by both the stmt/eval plane
    /// (`preamble::pragmas_and_imports`) and the decl plane
    /// (`preamble::session_decl_module_env`) via ONE fold over the effect
    /// list, in list order — the single source for what used to be two
    /// hand-mirrored `type_name == "..."` gates (friction #23: they drifted).
    /// Empty for every effect that needs nothing beyond the fixed surface.
    pub extra_imports: &'static [&'static str],
    /// Thin curried helper definitions emitted after the `type M` alias.
    /// Each string is one or more lines of Haskell (signature + definition).
    pub helpers: &'static [&'static str],
    /// Type parameters the GADT head carries BEFORE its result parameter —
    /// `["v"]` for `data Finalize v a where`, empty for every unparameterized
    /// effect. A parameterized effect's row entry is APPLIED (`Finalize
    /// Decision`), so `Member (Finalize T)` is what admits `finalize @T`:
    /// the row itself is the constraint, exactly as `State s` works.
    pub type_params: &'static [&'static str],
    /// The row arguments a compile that supplies none falls back to — same
    /// length as `type_params`, empty when there are none. `Finalize`'s is the
    /// uninhabited `NoAnswer` it declares in its own `type_defs`: a turn that
    /// is not answering a typed hole cannot finalize at all, and GHC says so
    /// by name (`'Finalize Text' is not a member of '[…, Finalize NoAnswer]'`).
    pub default_row_args: &'static [&'static str],
    /// Do this effect's `helpers` typecheck against ANY row that carries the
    /// `Member <Effect> effs` constraint, rather than only the closed `M`
    /// alias? A row-polymorphic helper (`foo :: Member E effs => A -> Eff
    /// effs B`, e.g. `RunLLMTurn`'s `runLLMTurn`) compiles fine even when `E`
    /// is absent from the CURRENT row — the constraint just goes unsolved
    /// until a call site fixes `effs`, so it's safe to emit for an effect
    /// that is in the generated module's VOCABULARY but not its ROW (see
    /// [`crate::effects_module_source_with_vocab`]). A row-CLOSED helper
    /// (`foo :: A -> M B`, the default) only typechecks when its effect is
    /// actually in the row — emitting it otherwise breaks the compile at the
    /// DEFINITION site, the opposite of the vocabulary/row split's goal — so
    /// such helpers stay row-gated. `false` for every effect except
    /// `RunLLMTurn`.
    pub helpers_row_polymorphic: bool,
}

/// The type arguments a single compile applies to the parameterized effects in
/// its row, plus the modules the generated `Tidepool.Effects` must import to
/// resolve them.
///
/// The answer type of a `Finalize` hole is an AUTHOR type (`Decision`,
/// `Contribution`), known only per-compile, so it cannot live in the `'static`
/// [`EffectDecl`]. It rides here instead: `RowArgs::at("Finalize",
/// ["Decision"]).importing(["HarnessTypes"])` renders the row entry `Finalize
/// Decision` and adds `import HarnessTypes` to the generated module (naming a
/// type requires it in scope THERE, not only in the turn module).
///
/// An empty `RowArgs` renders every effect at its [`EffectDecl::default_row_args`]
/// — the shape every non-harness stack (`standard_decls()`, which carries no
/// parameterized effect at all) already had.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowArgs {
    args: std::collections::BTreeMap<String, Vec<String>>,
    imports: Vec<String>,
}

impl RowArgs {
    /// Apply `args` to `effect`'s row entry.
    pub fn at<I, S>(effect: &str, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut m = Self::default();
        m.args.insert(
            effect.to_string(),
            args.into_iter().map(Into::into).collect(),
        );
        m
    }

    /// Add the modules the generated `Tidepool.Effects` must import for the
    /// applied types to resolve.
    #[must_use]
    pub fn importing<I, S>(mut self, modules: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for m in modules {
            let m = m.into();
            if !self.imports.contains(&m) {
                self.imports.push(m);
            }
        }
        self
    }

    /// The arguments applied to `effect`, or `None` to use its default.
    #[must_use]
    pub fn get(&self, effect: &str) -> Option<&[String]> {
        self.args.get(effect).map(Vec::as_slice)
    }

    /// The extra imports the generated module needs.
    #[must_use]
    pub fn imports(&self) -> &[String] {
        &self.imports
    }
}

/// Render one entry of a promoted effect row: `Console`, `Finalize Decision`,
/// `Finalize (Int -> Int)`.
///
/// An argument that isn't a single atom is parenthesized, so a function or
/// applied type (`Int -> Int`, `Maybe Text`) stays one row element.
#[must_use]
pub fn row_entry(decl: &EffectDecl, row: &RowArgs) -> String {
    let mut out = String::from(decl.type_name);
    match row.get(decl.type_name) {
        Some(args) => {
            debug_assert_eq!(
                args.len(),
                decl.type_params.len(),
                "{}: row arguments must saturate its type parameters",
                decl.type_name
            );
            for a in args {
                out.push(' ');
                out.push_str(&parenthesize(a));
            }
        }
        None => {
            for d in decl.default_row_args {
                out.push(' ');
                out.push_str(&parenthesize(d));
            }
        }
    }
    out
}

/// Wrap a type argument in parens unless it is a single atom.
fn parenthesize(ty: &str) -> String {
    let t = ty.trim();
    if t.chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '\'')
        || (t.starts_with('(') && t.ends_with(')'))
        || (t.starts_with('[') && t.ends_with(']'))
    {
        t.to_string()
    } else {
        format!("({t})")
    }
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

// AskUser effect (self-iterating-harness Wave 2, answerer-only): `askuser_decl()`
// is generated from the single-source definition (`effect_defs.rs`).
crate::askuser_effect_def!(crate::effect_defs::effect_decl_projection);

// RunLLMTurn effect (self-iterating-harness WS-B, split out of Ask):
// `runllmturn_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::runllmturn_effect_def!(crate::effect_defs::effect_decl_projection);

// Finalize effect (self-iterating-harness WS-B): `finalize_decl()` is
// generated from the single-source definition (`effect_defs.rs`).
crate::finalize_effect_def!(crate::effect_defs::effect_decl_projection);

// Fork effect (answerer parallel-delegation surface): `fork_decl()` is
// generated from the single-source definition (`effect_defs.rs`).
crate::fork_effect_def!(crate::effect_defs::effect_decl_projection);

// Llm effect: `llm_decl()` is generated from the single-source definition
// (`effect_defs.rs`).
crate::llm_effect_def!(crate::effect_defs::effect_decl_projection);

// Worktree / RepoEvent (PRD 19): `worktree_decl()` / `event_decl()`. These are
// NOT in `build_base_stack`'s row — the dev-tree dogfood that would put them
// there is on hold pending the agent lane. They exist as decls so a caller
// that wants managed worktrees and typed repository events can build a row
// containing them, which is what lane L4's acceptance harness does.
crate::worktree_effect_def!(crate::effect_defs::effect_decl_projection);
crate::event_effect_def!(crate::effect_defs::effect_decl_projection);

// Subagent (PRD 18 lane 1): `subagent_decl()`. Like Worktree/RepoEvent, NOT
// in `build_base_stack`'s row — an opt-in effect for rows that spawn coupled
// agent+worktree workers. Requires Worktree in the same row (its types
// reference WorktreeSpec/WorktreeHandle/WorktreeError).
crate::subagent_effect_def!(crate::effect_defs::effect_decl_projection);
