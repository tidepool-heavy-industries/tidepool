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
    /// text consumer help is assembled from.
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
    /// `askUserRaw`). Emitted by both the statement and evaluation modules
    /// (`preamble::pragmas_and_imports`) and the persistent declaration environment
    /// (`preamble::session_decl_module_env`) via ONE fold over the effect
    /// list, in list order — the single source for what used to be two
    /// hand-mirrored `type_name == "..."` gates (friction #23: they drifted).
    /// Empty for every effect that needs nothing beyond the fixed surface.
    pub extra_imports: &'static [&'static str],
    /// Row-polymorphic curried helper definitions emitted with the vocabulary.
    /// Each string is one or more lines of Haskell (signature + definition).
    pub helpers: &'static [&'static str],
    /// Type parameters the GADT head carries before its result parameter,
    /// e.g. `["api"]` for `ActorLocal api a`. A row entry applies these
    /// parameters, so `Member (ActorLocal Maybe) effs` admits that protocol.
    pub type_params: &'static [&'static str],
    /// The row arguments a compile that supplies none falls back to — same
    /// length as `type_params`, empty when there are none. These arguments
    /// must match each parameter's kind, as with `ActorLocal Maybe`.
    pub default_row_args: &'static [&'static str],
}

/// The type arguments a single compile applies to the parameterized effects in
/// its row, plus the modules the invocation must import to resolve them.
///
/// Protocol types known per compile live here rather than in the static
/// declaration. `RowArgs::at("ActorLocal", ["Protocol"]).importing(["ActorTypes"])`
/// renders `ActorLocal Protocol` and imports the module defining the protocol.
/// An empty `RowArgs` uses each declaration's default arguments.
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

    /// Add modules to the invocation preamble so its applied row types resolve.
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

    /// The extra imports a checked invocation needs to name these row arguments.
    #[must_use]
    pub fn imports(&self) -> &[String] {
        &self.imports
    }
}

/// Render one entry of a promoted effect row: `Console`, `ActorLocal Maybe`,
/// `ActorLocal (Either Bool)`.
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

/// Support emitted by an installed interpreter instance, independent of the
/// authored row and the caller's authority. Inert or unavailable instances
/// return no keys even when their static Haskell vocabulary exists.
pub trait InstalledEffectSupport {
    fn installed_effect_support(&self) -> Vec<exomonad_tool::ToolEffectKey>;

    /// Families this instance recognizes, including inert implementations.
    /// Nominal dispatch stops at the first recognizer in the installed stack.
    fn handled_effect_families(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        self.installed_effect_support()
    }
}

impl InstalledEffectSupport for frunk::HNil {
    fn installed_effect_support(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        Vec::new()
    }
}

impl<H: InstalledEffectSupport, T: InstalledEffectSupport> InstalledEffectSupport
    for frunk::HCons<H, T>
{
    fn installed_effect_support(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        let mut keys = self.head.installed_effect_support();
        let shadowed = self.head.handled_effect_families();
        for key in self.tail.installed_effect_support() {
            if !shadowed.contains(&key) && !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }

    fn handled_effect_families(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        let mut keys = self.head.handled_effect_families();
        for key in self.tail.handled_effect_families() {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }
}

impl<H: InstalledEffectSupport> InstalledEffectSupport for std::sync::Arc<H> {
    fn installed_effect_support(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        (**self).installed_effect_support()
    }

    fn handled_effect_families(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        (**self).handled_effect_families()
    }
}

impl<H: InstalledEffectSupport> InstalledEffectSupport for parking_lot::Mutex<H> {
    fn installed_effect_support(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        self.lock().installed_effect_support()
    }

    fn handled_effect_families(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        self.lock().handled_effect_families()
    }
}

// ---------------------------------------------------------------------------
// Standard effect declarations
// ---------------------------------------------------------------------------

// Console is generated from the protocol owner.
crate::kv_effect_def!(crate::effect_defs::effect_decl_projection);
crate::fs_read_effect_def!(crate::effect_defs::effect_decl_projection);
crate::fs_write_effect_def!(crate::effect_defs::effect_decl_projection);
crate::http_effect_def!(crate::effect_defs::effect_decl_projection);

// Exec: MIGRATED — `exec_decl()` comes from `src/generated/exec.rs`.

crate::git_effect_def!(crate::effect_defs::effect_decl_projection);
crate::time_effect_def!(crate::effect_defs::effect_decl_projection);
crate::entropy_effect_def!(crate::effect_defs::effect_decl_projection);
crate::meta_effect_def!(crate::effect_defs::effect_decl_projection);
crate::ask_effect_def!(crate::effect_defs::effect_decl_projection);

// AskUser and ReadState declarations come from src/generated/.

crate::llm_effect_def!(crate::effect_defs::effect_decl_projection);

// Worktree, RepoEvent, Journal, and Green are opt-in effects outside the base row.
// Their declarations come from src/generated/.
