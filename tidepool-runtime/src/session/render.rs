//! Pure rendering of the gen-versioned `Tidepool.Session.Lib.G<g>` declaration
//! modules (Lane A, plan §3 + §5.0).
//!
//! The whole module source is a **pure function of the decl log**: given the
//! ordered turns (each carrying the raw declaration source text and the
//! GHC-sourced [`ExportItem`]s it introduces), [`render_module`] produces the
//! source of any one generation's module. Each generation imports the prior
//! generation **selectively** — `import …G<g-1> hiding (<names redefined this
//! turn>)` — and re-exports it plus this turn's items. That selective re-export
//! is what lets a redefined `data` type coexist with its older shape without
//! GHC's conflicting-export error (kimi-r2 #2): the two `Foo`s live in distinct
//! gen-versioned modules and only the newest is in scope unqualified.
//!
//! Binder names come from GHC (see `super::binders`), never a Rust-side Haskell
//! parser — this module only *renders* the structured items.

use serde_json::Value as Json;
use tidepool_repr::{Generation, SessionModule};

/// A name a declaration turn brings into scope, as classified by GHC.
///
/// `Value` is a function/value binder (`slug`). `Type` is a type/data
/// constructor head plus its data-constructor children (`Foo` with `[A, B]`),
/// so it can be rendered as `Foo(..)` for both export and `hiding`. `Class`
/// is a typeclass head with its method names, rendered as `Class(..)` so
/// that later `instance` declarations can see the methods.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportItem {
    Value { name: String },
    Type { name: String, cons: Vec<String> },
    Class { name: String, methods: Vec<String> },
}

/// Parenthesize an operator name for an export/`hiding` list (`.+` → `(.+)`).
/// Normal identifiers (alphanumeric/`_`-leading) and already-parenthesized names
/// pass through unchanged. Without this, a `session_def`'d operator like `(.+)`
/// emits `module …Lib.G1 (.+) where` with the operator UNPARENTHESIZED in the
/// export list — a GHC parse error that broke the whole session.
fn op_wrap(name: &str) -> String {
    match name.chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' || c == '(' => name.to_string(),
        _ => format!("({name})"),
    }
}

impl ExportItem {
    /// The head identifier (the value name, the type/class name).
    #[must_use]
    pub fn head_name(&self) -> &str {
        match self {
            ExportItem::Value { name }
            | ExportItem::Type { name, .. }
            | ExportItem::Class { name, .. } => name,
        }
    }

    /// Every identifier this item introduces: the head plus constructors or methods.
    /// Used to decide whether a later turn redefines (shadows) this item.
    pub fn all_names(&self) -> impl Iterator<Item = &str> {
        let head = std::iter::once(self.head_name());
        let cons: Box<dyn Iterator<Item = &str>> = match self {
            ExportItem::Type { cons, .. } => Box::new(cons.iter().map(String::as_str)),
            ExportItem::Class { methods, .. } => Box::new(methods.iter().map(String::as_str)),
            ExportItem::Value { .. } => Box::new(std::iter::empty()),
        };
        head.chain(cons)
    }

    /// Render this item as an export-list / `hiding`-list entry. A value is its
    /// bare name; a type with constructors exports via `(..)` so the value shape
    /// stays usable; a type synonym (no constructors) renders as a bare head
    /// (GHC rejects `Synonym(..)` for synonyms); a class always renders as
    /// `Class(..)` so later instances can see its methods.
    #[must_use]
    pub fn render_entry(&self) -> String {
        match self {
            ExportItem::Value { name } => op_wrap(name),
            // A type synonym or family (empty cons) renders as a bare head;
            // `(..)` is rejected by GHC for synonyms.
            ExportItem::Type { name, cons } if cons.is_empty() => op_wrap(name),
            ExportItem::Type { name, .. } => format!("{}(..)", op_wrap(name)),
            // A class always exports with `(..)` so methods are visible to instances.
            ExportItem::Class { name, .. } => format!("{}(..)", op_wrap(name)),
        }
    }

    /// Parse one item from the binder-extractor JSON object
    /// (`{"kind":"value","name":"slug"}` / `{"kind":"type","name":"Foo",
    /// "cons":["A","B"]}` / `{"kind":"class","name":"C","methods":["m"]}`).
    /// Returns `None` for a malformed entry.
    pub(crate) fn from_json(v: &Json) -> Option<ExportItem> {
        let kind = v.get("kind")?.as_str()?;
        let name = v.get("name")?.as_str()?.to_string();
        match kind {
            "value" => Some(ExportItem::Value { name }),
            "type" => {
                let cons = v
                    .get("cons")
                    .and_then(Json::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                Some(ExportItem::Type { name, cons })
            }
            "class" => {
                let methods = v
                    .get("methods")
                    .and_then(Json::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                Some(ExportItem::Class { name, methods })
            }
            _ => None,
        }
    }
}

/// One declaration turn: the raw source text(s) appended this turn and the
/// export items GHC says they introduce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclTurn {
    /// Raw declaration source as the user wrote it (verbatim — its faithful
    /// form is its source). May contain several top-level decls.
    pub sources: Vec<String>,
    /// The exportable binders this turn introduces (from GHC).
    pub items: Vec<ExportItem>,
    /// Names this turn REMOVES from the decl plane (no replacement). A name is
    /// retracted when its binding migrates to the value plane (e.g. a
    /// self-referential `n <- pure (n+1)` that must materialize) — the decl
    /// plane must then stop exporting it, or a later `let`/`def` would compile
    /// against the stale decl. A pure-retraction turn carries empty
    /// `sources`/`items` and one or more `retracts`. Honored by every scoping
    /// fold (`cumulative_exports_before`, `current_heads`, `replayable_sources`,
    /// `render_module`) so all decl-plane views stay consistent; a later
    /// `define` of the same name naturally un-retracts it (latest-wins).
    pub retracts: Vec<String>,
}

/// The ordered declaration log. `turns[i]` is generation `i + 1`
/// (`Generation(0)` is the empty session, with no module).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeclLog {
    pub turns: Vec<DeclTurn>,
}

impl DeclLog {
    #[must_use]
    pub fn new() -> DeclLog {
        DeclLog { turns: Vec::new() }
    }

    /// The current generation = number of turns recorded. `Generation(0)` until
    /// the first declaration.
    #[must_use]
    pub fn generation(&self) -> Generation {
        Generation(self.turns.len() as u64)
    }

    /// Append a turn, returning the new (current) generation.
    pub fn push(&mut self, turn: DeclTurn) -> Generation {
        self.turns.push(turn);
        self.generation()
    }

    /// The currently in-scope declaration heads (value/type/class names) paired
    /// with the generation of their LATEST defining turn (latest-wins across
    /// turns, mirroring the eval-time module scoping). Backs the decl-plane half
    /// of the `tidepool://session/bindings` live-state snapshot.
    #[must_use]
    pub fn current_heads(&self) -> Vec<(String, u64)> {
        let mut map: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        for (i, turn) in self.turns.iter().enumerate() {
            let gen = (i + 1) as u64;
            for r in &turn.retracts {
                map.remove(r);
            }
            for item in &turn.items {
                map.insert(item.head_name().to_string(), gen);
            }
        }
        map.into_iter().collect()
    }

    /// The declaration source texts of a **replayable** notebook skeleton: turn
    /// sources in log order, but with fully-superseded turns dropped so a name
    /// redefined across SEPARATE turns emits only its LATEST definition (#320).
    ///
    /// A flat `:program` replay concatenates top-level decls, so two turns that
    /// both define `rf` (`rf x = x+1` then `rf x = x+2`) would emit two
    /// conflicting `rf` equations — an overlapping-clause pair GHC rejects as
    /// "multiple declarations of rf". The eval-time scope already resolves this
    /// latest-wins (see [`cumulative_exports_before`]); this mirrors that rule
    /// for the flat repaint.
    ///
    /// The rule matches the module scoping: a turn is dropped iff **every** head
    /// it introduces (by head name) is redefined in a *later* turn. Consequences:
    /// - Cross-turn redefinition (`rf` then `rf`): the earlier turn is fully
    ///   superseded → dropped; only the latest `rf` source is emitted.
    /// - A genuine multi-clause function in ONE turn (`f 0 = ..\nf n = ..`) is a
    ///   single source with one head → never self-supersedes → emitted verbatim,
    ///   both clauses preserved.
    /// - A turn with no exportable head (e.g. a bare `instance`) is always kept.
    ///
    /// Limitation (matches the "one declaration per item" idiom's blind spot): a
    /// turn co-defining a later-redefined head *and* a still-live head is NOT
    /// fully superseded, so it is kept — the live head is preserved faithfully,
    /// but the co-defined stale head can still duplicate in the flat replay. The
    /// runtime resolves that via the gen-versioned module split; a flat skeleton
    /// cannot, short of re-parsing the source per-binder. Single-head turns (the
    /// documented norm) never hit this.
    #[must_use]
    pub fn replayable_sources(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for (i, turn) in self.turns.iter().enumerate() {
            let heads: Vec<&str> = turn.items.iter().map(ExportItem::head_name).collect();
            // A turn's source is dropped once every head it introduced is later
            // redefined OR retracted — the migrated/superseded decl must not
            // reappear in a flat `:program` replay.
            let fully_superseded = !heads.is_empty()
                && heads.iter().all(|h| {
                    self.turns[i + 1..].iter().any(|later| {
                        later.items.iter().any(|it| it.head_name() == *h)
                            || later.retracts.iter().any(|r| r == h)
                    })
                });
            if !fully_superseded {
                out.extend(turn.sources.iter().map(String::as_str));
            }
        }
        out
    }
}

/// The import preamble the generated session module needs so user declarations
/// type-check (the same surface evals see). Held as a parameter so the
/// standalone Lane-A test uses a small pure surface while the full server can
/// later pass the effects/`M`-stack preamble unchanged.
#[derive(Clone, Debug)]
pub struct ModuleEnv {
    /// The `{-# LANGUAGE … #-}` pragma block (one line, no trailing newline).
    pub pragmas: String,
    /// Import lines (without the leading `import` keyword is NOT assumed —
    /// each entry is a full `import …` line), emitted before the prior-gen import.
    pub imports: Vec<String>,
}

impl ModuleEnv {
    /// A minimal **lens-free** pure surface sufficient for Lane-A declarations:
    /// the JIT-safe `T.` text vocabulary (`Tidepool.Data.Text`) and `Map.`, over
    /// the base `Prelude`. Deliberately avoids `Tidepool.Prelude` (which pulls
    /// `Control.Lens`, demanding the `with-packages` GHC) so the standalone
    /// declaration REPL compiles against the plain toolchain. The full server
    /// (Wave 2) passes its own effects/`M`-stack [`ModuleEnv`] instead.
    #[must_use]
    pub fn standalone_default() -> ModuleEnv {
        ModuleEnv {
            // NoMonomorphismRestriction: pure binds routed as decls must
            // generalize (see EVAL_PRAGMAS note in tidepool-mcp).
            //
            // MUST track the production decl module's extension set
            // (`tidepool_mcp::decl_pragmas` = EVAL_PRAGMAS + NMR) so this
            // test/standalone default is FAITHFUL — else a decl valid in
            // production fails here (or vice-versa). The layer boundary
            // (runtime can't see mcp) blocks sharing the list, so it is
            // mirrored here; the sole intentional divergence is
            // `NoImplicitPrelude` (standalone relies on the implicit Prelude,
            // via its plain-toolchain imports below). `OverloadedRecordDot` +
            // `DuplicateRecordFields` were MISSING — record-dot (`h.path`, a
            // core idiom) compiled live but not here (friction #28).
            pragmas: "{-# LANGUAGE OverloadedStrings, NoMonomorphismRestriction, DataKinds, TypeOperators, \
                      FlexibleContexts, FlexibleInstances, UndecidableInstances, GADTs, \
                      PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, \
                      LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, \
                      ViewPatterns, BangPatterns, TypeApplications, BlockArguments, \
                      NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, \
                      DeriveTraversable, QuasiQuotes, DuplicateRecordFields, OverloadedRecordDot #-}"
                .to_string(),
            imports: vec![
                "import qualified Tidepool.Data.Text as T".to_string(),
                "import qualified Data.Map.Strict as Map".to_string(),
            ],
        }
    }
}

/// A rendered session-library module: its name and source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedModule {
    pub module: SessionModule,
    pub source: String,
    /// Number of generated lines (pragmas, header, imports) before the user's
    /// declaration text — the offset for mapping GHC's line numbers back to
    /// item-relative ones (see [`super::errmap`]).
    pub body_line: usize,
    /// True when pragma/import hoisting REMOVED lines from the user's source,
    /// making the offset mapping inexact — coordinate remapping is skipped.
    pub hoisted_lines: bool,
}

/// Parse `{-# LANGUAGE Ext1, Ext2 #-}` (single pragma block, one or more lines)
/// into the list of extension names. Returns an empty vec for absent or malformed input.
fn parse_pragma_extensions(pragma_block: &str) -> Vec<String> {
    let mut exts = Vec::new();
    for line in pragma_block.lines() {
        let line = line.trim();
        if line.starts_with("{-# LANGUAGE") && line.contains("#-}") {
            if let Some(inner) = line
                .strip_prefix("{-# LANGUAGE")
                .and_then(|s| s.rfind("#-}").map(|i| s[..i].trim()))
            {
                for e in inner.split(',') {
                    let e = e.trim().to_string();
                    if !e.is_empty() {
                        exts.push(e);
                    }
                }
            }
        }
    }
    exts
}

/// Scan `src` line by line for `{-# LANGUAGE … #-}` pragmas (line-anchored),
/// collect their extension names, strip those lines, and return
/// `(stripped_source, extensions)`.
fn extract_language_pragmas(src: &str) -> (String, Vec<String>) {
    let mut exts = Vec::new();
    let mut kept: Vec<&str> = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("{-# LANGUAGE") && t.contains("#-}") {
            if let Some(inner) = t
                .strip_prefix("{-# LANGUAGE")
                .and_then(|s| s.rfind("#-}").map(|i| s[..i].trim()))
            {
                for e in inner.split(',') {
                    let e = e.trim().to_string();
                    if !e.is_empty() {
                        exts.push(e);
                    }
                }
            }
        } else {
            kept.push(line);
        }
    }
    (kept.join("\n"), exts)
}

/// Scan `src` line by line for top-level `import …` lines (line-anchored),
/// collect them verbatim, strip those lines, and return
/// `(stripped_source, import_lines)`.
///
/// Handles the common single-line case: `import [qualified] Mod [as X]
/// [(list)] [hiding (list)]`. Multi-line import lists (a line ending with `(`
/// and no closing `)` on the same line) are not currently hoisted — they stay
/// in the body where they are a parse error if before declarations. Multi-line
/// imports in session_def bodies are rare enough that single-line coverage
/// handles the practical cases.
fn extract_user_imports(src: &str) -> (String, Vec<String>) {
    let mut imports = Vec::new();
    let mut kept: Vec<&str> = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("import ") || t.starts_with("import\t") {
            imports.push(t.to_string());
        } else {
            kept.push(line);
        }
    }
    (kept.join("\n"), imports)
}

/// Rewrite one `env.imports` line so it also hides every name in
/// `all_session_heads`, merging into any `hiding (…)` clause the line already
/// carries (e.g. `import Tidepool.Prelude hiding (error)`) rather than
/// duplicating the clause (GHC allows only one `hiding` per import). A
/// `import qualified …` line is returned unchanged — qualified names can
/// never collide with an unqualified session decl. Empty `all_session_heads`
/// also returns the line unchanged (no session decls yet to guard against).
fn hide_session_heads(imp: &str, all_session_heads: &[&ExportItem]) -> String {
    if all_session_heads.is_empty() || imp.trim_start().starts_with("import qualified") {
        return imp.to_string();
    }
    let mut hides: Vec<String> = all_session_heads.iter().map(|p| p.render_entry()).collect();
    let base = if let Some(hiding_at) = imp.find(" hiding (") {
        let prefix = &imp[..hiding_at];
        let list_start = hiding_at + " hiding (".len();
        if let Some(rel_end) = imp[list_start..].find(')') {
            let existing = &imp[list_start..list_start + rel_end];
            hides.extend(
                existing
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from),
            );
        }
        prefix.to_string()
    } else {
        imp.to_string()
    };
    hides.sort();
    hides.dedup();
    format!("{base} hiding ({})", hides.join(", "))
}

/// The export items in scope (and re-exported) by `Lib.G<g-1>`, i.e. after
/// applying shadowing across turns `0..g-1`. A later turn redefines a prior item
/// iff their **head names** match (a function redefines a same-named function; a
/// `data Foo` redefines a prior `data Foo` — the spec's reshape case), removing
/// the prior item before adding its own (latest-wins).
///
/// Matching is deliberately **head-name only**, NOT every introduced identifier:
/// hiding on any shared constructor name would silently nuke an *unrelated*
/// prior type that merely reused a constructor name. Cross-type constructor
/// reuse instead surfaces as a loud GHC conflicting-export error at compile —
/// the honest outcome for a genuinely ambiguous program (and out of Lane A's
/// scope). Reshape-coexistence of a redefined type lives in the gen-versioned
/// module split, not here.
fn cumulative_exports_before(log: &DeclLog, gen_one_based: usize) -> Vec<ExportItem> {
    let mut acc: Vec<ExportItem> = Vec::new();
    for turn in log.turns.iter().take(gen_one_based.saturating_sub(1)) {
        let new_heads: Vec<&str> = turn.items.iter().map(ExportItem::head_name).collect();
        // A turn removes prior exports it either redefines OR retracts; then
        // re-adds its own. (A retraction adds nothing.)
        acc.retain(|prior| {
            !new_heads.contains(&prior.head_name())
                && !turn.retracts.iter().any(|r| r == prior.head_name())
        });
        acc.extend(turn.items.iter().cloned());
    }
    acc
}

/// Render generation `gen` (1-based; must be `1..=log.turns.len()`) as a
/// `Tidepool.Session.Lib.G<gen>` module. Panics on an out-of-range generation
/// (a caller bug — generations only ever count existing turns).
///
/// `shadow_wildcard_imports`: whether this turn's (and prior turns') decl
/// heads get hidden from every unqualified `env.imports` line (`Library`,
/// `Tidepool.Prelude`, …) so a decl reusing a name those modules also export
/// (e.g. `over`, which `Tidepool.Prelude` re-exports from `Control.Lens`)
/// shadows gracefully instead of an "ambiguous occurrence" — GHCi parity for
/// a genuine top-level declaration. Pass `false` for a pure `let`/`<-` bind
/// promoted into a decl only for GHCi-parity type generalization (see
/// `tidepool-repl`'s `try_pure_bind_as_decl`): there, a collision with a
/// wildcard-imported name is a real warning sign, not an intentional
/// redefinition, so it should surface as a loud compile error instead of
/// silently shadowing.
#[must_use]
pub fn render_module(
    log: &DeclLog,
    gen: Generation,
    env: &ModuleEnv,
    shadow_wildcard_imports: bool,
) -> RenderedModule {
    let g = gen.0 as usize;
    assert!(
        g >= 1 && g <= log.turns.len(),
        "render_module: generation {g} out of range 1..={}",
        log.turns.len()
    );
    let module = SessionModule::lib(gen);
    let this = &log.turns[g - 1];
    let prior = cumulative_exports_before(log, g);

    // Hoist LANGUAGE pragmas and import lines from user source: strip them so
    // they don't reappear after the module header / in the declaration body.
    let mut hoisted_exts: Vec<String> = Vec::new();
    let mut hoisted_imports: Vec<String> = Vec::new();
    let stripped_sources: Vec<String> = this
        .sources
        .iter()
        .map(|src| {
            let (stripped, exts) = extract_language_pragmas(src);
            hoisted_exts.extend(exts);
            let (stripped, imps) = extract_user_imports(&stripped);
            hoisted_imports.extend(imps);
            stripped
        })
        .collect();

    // Build merged pragma block: env extensions first, then any new user
    // extensions not already present (dedupe, order-preserving).
    let mut merged_exts = parse_pragma_extensions(&env.pragmas);
    for ext in &hoisted_exts {
        if !merged_exts.contains(ext) {
            merged_exts.push(ext.clone());
        }
    }
    let merged_pragmas = if merged_exts.is_empty() {
        env.pragmas.clone()
    } else {
        format!("{{-# LANGUAGE {} #-}}", merged_exts.join(", "))
    };

    // Heads this turn (re)defines — drives the `hiding` clause on the prior-gen
    // import (head-name match only; see `cumulative_exports_before`).
    let new_heads: Vec<&str> = this.items.iter().map(ExportItem::head_name).collect();
    // Hide from the prior-gen import every head this turn REDEFINES or RETRACTS.
    // For a retraction the name is still in `prior` (retracts take effect for
    // LATER gens via `cumulative_exports_before`); hiding it here drops it from
    // this gen's `import Prev hiding (…)` and — since `module Prev` only
    // re-exports in-scope names — from the re-export too, so the name is gone.
    let hidden_prior: Vec<&ExportItem> = prior
        .iter()
        .filter(|p| {
            new_heads.contains(&p.head_name()) || this.retracts.iter().any(|r| r == p.head_name())
        })
        .collect();

    // Every head this session has ever (re)defined, prior gens + this turn —
    // guards every UNQUALIFIED `env.imports` line against colliding with a
    // session's own decls (see `hide_session_heads` below). Without this, a
    // decl defining a name some wildcard-imported module also exports (e.g.
    // `data Hit` vs. the project `Library` facade's `Hit`, or `over` vs.
    // `Tidepool.Prelude`'s Control.Lens re-export) becomes an "ambiguous
    // occurrence" — the same collision class `hide_module_names`
    // (tidepool-repl) already guards on the stmt-preamble side (BUG-7); this
    // ports the same guard to EVERY unqualified decl-module import (not just
    // `Library`) now that the decl env always carries the full
    // `Tidepool.Prelude` surface (see `session_decl_module_env`). Empty when
    // `shadow_wildcard_imports` is false, so a pure-bind-promoted decl gets NO
    // shadowing and a collision surfaces loudly instead.
    let all_session_heads: Vec<&ExportItem> = if shadow_wildcard_imports {
        prior.iter().chain(this.items.iter()).collect()
    } else {
        Vec::new()
    };

    let prev_module = if g >= 2 {
        Some(SessionModule::lib(Generation((g - 1) as u64)))
    } else {
        None
    };

    let mut out = String::new();
    out.push_str(&merged_pragmas);
    out.push('\n');
    out.push_str(
        "-- GENERATED (Lane A) — accumulated session declarations. Do not edit;\n\
         -- regenerated as a pure function of the declaration log each turn.\n",
    );

    // Export list: re-export the prior gen (its non-hidden names) selectively,
    // then this turn's items explicitly.
    let mut exports: Vec<String> = Vec::new();
    if let Some(prev) = prev_module {
        exports.push(format!("module {}", prev.module_name()));
    }
    for item in &this.items {
        exports.push(item.render_entry());
    }
    out.push_str(&format!("module {} (", module.module_name()));
    if exports.is_empty() {
        out.push_str(") where\n");
    } else {
        out.push('\n');
        for (i, e) in exports.iter().enumerate() {
            let comma = if i + 1 < exports.len() { "," } else { "" };
            out.push_str(&format!("    {e}{comma}\n"));
        }
        out.push_str("  ) where\n");
    }

    // Standard imports, then the selective prior-gen import, then any user
    // imports hoisted from decl sources (deduped against the standard set).
    for imp in &env.imports {
        out.push_str(&hide_session_heads(imp, &all_session_heads));
        out.push('\n');
    }
    if let Some(prev) = prev_module {
        if hidden_prior.is_empty() {
            out.push_str(&format!("import {}\n", prev.module_name()));
        } else {
            let hides: Vec<String> = hidden_prior.iter().map(|p| p.render_entry()).collect();
            out.push_str(&format!(
                "import {} hiding ({})\n",
                prev.module_name(),
                hides.join(", ")
            ));
        }
    }
    for imp in &hoisted_imports {
        if !env.imports.contains(imp) {
            out.push_str(imp);
            out.push('\n');
        }
    }
    out.push('\n');

    // Lines emitted before the user's declaration text — the coordinate-remap
    // offset. Hoisting that DELETED lines from the user source breaks the
    // 1:1 line mapping; flag it so the remap is skipped rather than wrong.
    let body_line = out.matches('\n').count();
    let hoisted_lines = stripped_sources
        .iter()
        .zip(&this.sources)
        .any(|(stripped, original)| stripped.lines().count() != original.lines().count());

    // The accumulated declaration source for this turn, with LANGUAGE pragmas
    // already hoisted into the merged pragma block above.
    for src in &stripped_sources {
        let body = src.trim_end();
        if !body.is_empty() {
            out.push_str(body);
            out.push_str("\n\n");
        }
    }

    RenderedModule {
        module,
        source: out,
        body_line,
        hoisted_lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val(name: &str) -> ExportItem {
        ExportItem::Value { name: name.into() }
    }
    fn ty(name: &str, cons: &[&str]) -> ExportItem {
        ExportItem::Type {
            name: name.into(),
            cons: cons.iter().map(|s| (*s).into()).collect(),
        }
    }
    fn turn(src: &str, items: Vec<ExportItem>) -> DeclTurn {
        DeclTurn {
            sources: vec![src.into()],
            items,
            retracts: Vec::new(),
        }
    }
    /// A pure-retraction turn: removes `names` from the decl plane, no source.
    fn retract_turn(names: &[&str]) -> DeclTurn {
        DeclTurn {
            sources: Vec::new(),
            items: Vec::new(),
            retracts: names.iter().map(|s| (*s).into()).collect(),
        }
    }

    #[test]
    fn operator_value_is_parenthesized_in_exports() {
        // A session_def'd operator like `(.+)` must export as `(.+)`, not a bare
        // `.+` (which is a GHC parse error in an export list).
        let mut log = DeclLog::new();
        log.push(turn("a .+ b = a + b", vec![val(".+")]));
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default(), true);
        assert!(
            r.source.contains("(.+)"),
            "operator export must be parenthesized:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("\n    .+\n"),
            "bare operator in export list:\n{}",
            r.source
        );
        // And the prior-gen `hiding` clause must parenthesize too (redefine `.+`).
        log.push(turn("a .+ b = a - b", vec![val(".+")]));
        let r2 = render_module(&log, Generation(2), &ModuleEnv::standalone_default(), true);
        assert!(
            r2.source.contains("hiding ((.+))"),
            "hiding clause must parenthesize:\n{}",
            r2.source
        );
    }

    #[test]
    fn first_gen_has_no_prior_import() {
        let mut log = DeclLog::new();
        log.push(turn("slug t = T.toLower t", vec![val("slug")]));
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default(), true);
        assert_eq!(r.module.module_name(), "Tidepool.Session.Lib.G1");
        assert!(r.source.contains("module Tidepool.Session.Lib.G1 ("));
        assert!(r.source.contains("    slug\n"));
        assert!(!r.source.contains("import Tidepool.Session.Lib.G0"));
        assert!(r.source.contains("slug t = T.toLower t"));
    }

    #[test]
    fn second_gen_reexports_prior_when_no_redef() {
        let mut log = DeclLog::new();
        log.push(turn("slug t = t", vec![val("slug")]));
        log.push(turn("shout t = T.toUpper t", vec![val("shout")]));
        let r = render_module(&log, Generation(2), &ModuleEnv::standalone_default(), true);
        // No redefinition → plain import + module re-export.
        assert!(r.source.contains("import Tidepool.Session.Lib.G1\n"));
        assert!(r.source.contains("module Tidepool.Session.Lib.G1,"));
        assert!(r.source.contains("    shout"));
        // No redefinition → the prior-gen import carries no `hiding` clause.
        assert!(!r.source.contains("Session.Lib.G1 hiding"));
    }

    #[test]
    fn redefined_function_is_hidden_from_prior_import() {
        let mut log = DeclLog::new();
        log.push(turn("slug t = t", vec![val("slug")]));
        log.push(turn("other t = t", vec![val("other")]));
        log.push(turn("slug t = T.replace \" \" \"-\" t", vec![val("slug")]));
        let r = render_module(&log, Generation(3), &ModuleEnv::standalone_default(), true);
        // G3 redefines slug → hide it from G2's re-export (latest-wins).
        assert!(r
            .source
            .contains("import Tidepool.Session.Lib.G2 hiding (slug)"));
        assert!(r.source.contains("module Tidepool.Session.Lib.G2,"));
        // `other` (defined at G2, not redefined) stays re-exported transitively.
        assert!(!r.source.contains("    other"));
    }

    #[test]
    fn redefined_data_type_hides_with_dotdot_no_conflict() {
        let mut log = DeclLog::new();
        log.push(turn("data Foo = A | B", vec![ty("Foo", &["A", "B"])]));
        log.push(turn(
            "data Foo = X | A | B",
            vec![ty("Foo", &["X", "A", "B"])],
        ));
        let r2 = render_module(&log, Generation(2), &ModuleEnv::standalone_default(), true);
        // The reshape hides the OLD Foo and its constructors, avoiding GHC's
        // conflicting-export error, and re-declares + exports the new shape.
        assert!(r2
            .source
            .contains("import Tidepool.Session.Lib.G1 hiding (Foo(..))"));
        assert!(r2.source.contains("    Foo(..)"));
        assert!(r2.source.contains("data Foo = X | A | B"));
        // G1 still renders standalone (old shape stays compilable / resolvable).
        let r1 = render_module(&log, Generation(1), &ModuleEnv::standalone_default(), true);
        assert!(r1.source.contains("data Foo = A | B"));
        assert!(r1.source.contains("    Foo(..)"));
    }

    #[test]
    fn unrelated_constructor_reuse_does_not_hide_prior_type() {
        // Regression: head-name matching only. A later turn reusing a prior
        // type's CONSTRUCTOR name in a *different* type must NOT hide the prior
        // type — that would silently drop `Foo` and its sibling `B`.
        let mut log = DeclLog::new();
        log.push(turn("data Foo = A | B", vec![ty("Foo", &["A", "B"])]));
        log.push(turn("data Bar = A | C", vec![ty("Bar", &["A", "C"])]));
        let r = render_module(&log, Generation(2), &ModuleEnv::standalone_default(), true);
        // Foo is NOT redefined → no `hiding (Foo(..))`; it stays re-exported.
        assert!(!r.source.contains("hiding (Foo(..))"));
        assert!(r.source.contains("import Tidepool.Session.Lib.G1\n"));
        assert!(r.source.contains("module Tidepool.Session.Lib.G1,"));
        assert!(r.source.contains("    Bar(..)"));
    }

    #[test]
    fn multi_item_turn_hides_only_redefined_heads() {
        // A turn that both redefines `slug` and adds a fresh `Greeter` type:
        // only `slug` is hidden from the prior import; the new type is added.
        let mut log = DeclLog::new();
        log.push(turn("slug t = t", vec![val("slug")]));
        log.push(turn(
            "slug t = T.toUpper t\ndata Greeter = Hi | Yo",
            vec![val("slug"), ty("Greeter", &["Hi", "Yo"])],
        ));
        let r = render_module(&log, Generation(2), &ModuleEnv::standalone_default(), true);
        assert!(r
            .source
            .contains("import Tidepool.Session.Lib.G1 hiding (slug)"));
        assert!(r.source.contains("    slug,"));
        assert!(r.source.contains("    Greeter(..)"));
    }

    #[test]
    fn replayable_sources_drops_cross_turn_redefinition() {
        // #320: `rf x = x+1` then `rf x = x+2` in SEPARATE turns. A flat
        // `:program` replay must emit ONLY the latest `rf`, not both (the two
        // equations would be an overlapping-clause pair GHC rejects).
        let mut log = DeclLog::new();
        log.push(turn("rf x = x + 1", vec![val("rf")]));
        log.push(turn("rf x = x + 2", vec![val("rf")]));
        let srcs = log.replayable_sources();
        assert_eq!(
            srcs,
            vec!["rf x = x + 2"],
            "only the latest rf definition survives the flat repaint"
        );
    }

    #[test]
    fn replayable_sources_preserves_multiclause_single_item() {
        // A GENUINE multi-clause function lives in ONE turn (one source, one
        // head). It must survive verbatim — both clauses — never treated as a
        // self-redefinition.
        let mut log = DeclLog::new();
        log.push(turn("f 0 = 0\nf n = n * f (n - 1)", vec![val("f")]));
        let srcs = log.replayable_sources();
        assert_eq!(srcs, vec!["f 0 = 0\nf n = n * f (n - 1)"]);
        // And it still stands when an unrelated later turn is added.
        log.push(turn("g y = y", vec![val("g")]));
        assert_eq!(
            log.replayable_sources(),
            vec!["f 0 = 0\nf n = n * f (n - 1)", "g y = y"],
        );
    }

    #[test]
    fn replayable_sources_keeps_unredefined_and_orders_stably() {
        // Interleaved: define a, define b, redefine a. Latest `a` and the sole
        // `b` survive, in log order; the stale first `a` is dropped.
        let mut log = DeclLog::new();
        log.push(turn("a = 1", vec![val("a")]));
        log.push(turn("b = 2", vec![val("b")]));
        log.push(turn("a = 3", vec![val("a")]));
        assert_eq!(log.replayable_sources(), vec!["b = 2", "a = 3"]);
    }

    #[test]
    fn replayable_sources_keeps_headless_turn() {
        // A turn with no exportable head (e.g. a bare instance) is always kept.
        let mut log = DeclLog::new();
        log.push(turn("instance Show Foo where show _ = \"foo\"", vec![]));
        assert_eq!(
            log.replayable_sources(),
            vec!["instance Show Foo where show _ = \"foo\""],
        );
    }

    #[test]
    fn type_synonym_renders_bare_not_dotdot() {
        let mut log = DeclLog::new();
        log.push(turn("type Name = T.Text", vec![ty("Name", &[])]));
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default(), true);
        assert!(r.source.contains("    Name\n"));
        assert!(!r.source.contains("Name(..)"));
    }

    #[test]
    fn user_language_pragma_hoisted_above_module_header() {
        let mut log = DeclLog::new();
        log.push(turn(
            "{-# LANGUAGE DeriveAnyClass #-}\ndata Foo = Foo deriving (Eq, Show)",
            vec![ty("Foo", &["Foo"])],
        ));
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default(), true);
        let pragma_pos = r.source.find("{-# LANGUAGE").expect("pragma block present");
        let module_pos = r
            .source
            .find("module Tidepool.Session.Lib.G1")
            .expect("module header present");
        assert!(
            pragma_pos < module_pos,
            "LANGUAGE pragma must precede module header"
        );
        // DeriveAnyClass must appear in the merged pragma block (before the module line).
        assert!(
            r.source[..module_pos].contains("DeriveAnyClass"),
            "DeriveAnyClass must be in merged pragma block"
        );
        // The original pragma line must NOT appear in the body (after module header).
        let body = &r.source[module_pos..];
        assert!(
            !body.contains("{-# LANGUAGE DeriveAnyClass #-}"),
            "LANGUAGE pragma must be stripped from body"
        );
        // The declaration body must still be present.
        assert!(r.source.contains("data Foo = Foo deriving (Eq, Show)"));
    }

    #[test]
    fn multiple_user_language_pragmas_deduplicated() {
        let mut log = DeclLog::new();
        // OverloadedStrings already in standalone_default — must not appear twice.
        log.push(turn(
            "{-# LANGUAGE DeriveGeneric #-}\n{-# LANGUAGE OverloadedStrings #-}\ndata Bar = Bar",
            vec![ty("Bar", &["Bar"])],
        ));
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default(), true);
        let module_pos = r
            .source
            .find("module Tidepool.Session.Lib.G1")
            .expect("module header present");
        let preamble = &r.source[..module_pos];
        // DeriveGeneric added; OverloadedStrings already present → no duplicate.
        assert!(preamble.contains("DeriveGeneric"));
        let count = preamble.matches("OverloadedStrings").count();
        assert_eq!(count, 1, "OverloadedStrings must appear exactly once");
    }

    #[test]
    fn user_import_hoisted_to_import_section() {
        // An `import` inside a session_def body must be lifted into the module
        // header (import section), not left in the declaration body — a
        // body-position import is a GHC parse error.
        let mut log = DeclLog::new();
        log.push(turn(
            "import Data.Char (toUpper)\ntoUpper' c = toUpper c",
            vec![val("toUpper'")],
        ));
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default(), true);
        let module_pos = r
            .source
            .find("module Tidepool.Session.Lib.G1")
            .expect("module header present");
        let import_pos = r
            .source
            .find("import Data.Char (toUpper)")
            .expect("import must appear in output");
        let decl_pos = r
            .source
            .find("toUpper' c = toUpper c")
            .expect("decl body present");
        // Import must be in the header region (after module line, before decl body).
        assert!(
            import_pos > module_pos,
            "import must appear after module header:\n{}",
            r.source
        );
        assert!(
            import_pos < decl_pos,
            "import must appear before declaration body:\n{}",
            r.source
        );
        // The import must NOT reappear inside the declaration body.
        assert!(
            !r.source[decl_pos..].contains("import Data.Char"),
            "import must be stripped from declaration body:\n{}",
            r.source
        );
    }

    #[test]
    fn export_item_from_json_roundtrips() {
        let j: Json = serde_json::from_str(
            r#"{"items":[{"kind":"value","name":"slug"},
                        {"kind":"type","name":"Foo","cons":["A","B"]}]}"#,
        )
        .unwrap();
        let items: Vec<ExportItem> = j["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(ExportItem::from_json)
            .collect();
        assert_eq!(items, vec![val("slug"), ty("Foo", &["A", "B"])]);
    }

    // --- Retraction (a name leaving the decl plane on decl→value migration) ---

    fn heads(log: &DeclLog) -> Vec<String> {
        log.current_heads().into_iter().map(|(h, _)| h).collect()
    }

    #[test]
    fn retraction_removes_name_from_every_scoping_view() {
        let mut log = DeclLog::new();
        log.push(turn("findings = []", vec![val("findings")]));
        log.push(turn("keep t = t", vec![val("keep")]));
        // Before retraction: both are live in every view.
        assert_eq!(heads(&log), vec!["findings", "keep"]);
        let before = cumulative_exports_before(&log, log.turns.len() + 1);
        assert!(before.iter().any(|e| e.head_name() == "findings"));

        // findings migrates to the value plane → retract it.
        log.push(retract_turn(&["findings"]));

        // current_heads, cumulative exports, and decl replay all drop it;
        // `keep` is untouched.
        assert_eq!(heads(&log), vec!["keep"]);
        let after = cumulative_exports_before(&log, log.turns.len() + 1);
        assert!(!after.iter().any(|e| e.head_name() == "findings"));
        assert!(after.iter().any(|e| e.head_name() == "keep"));
        // The migrated decl's source is dropped from a flat replay.
        let replay = log.replayable_sources();
        assert!(!replay.iter().any(|s| s.contains("findings = []")));
        assert!(replay.iter().any(|s| s.contains("keep t = t")));
    }

    #[test]
    fn retraction_turn_hides_name_from_rendered_module() {
        let mut log = DeclLog::new();
        log.push(turn("findings = []", vec![val("findings")]));
        log.push(turn("keep t = t", vec![val("keep")]));
        log.push(retract_turn(&["findings"]));
        let r = render_module(&log, Generation(3), &ModuleEnv::standalone_default(), true);
        // The retraction shell hides `findings` from the prior-gen import (so
        // `module Prev` no longer re-exports it) and adds no new decl for it.
        assert!(
            r.source.contains("Session.Lib.G2 hiding (findings)"),
            "retraction must hide the name from the prior import:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("\n    findings"),
            "retracted name must not appear in the export list:\n{}",
            r.source
        );
    }

    #[test]
    fn define_after_retraction_unretracts_latest_wins() {
        let mut log = DeclLog::new();
        log.push(turn("findings = []", vec![val("findings")]));
        log.push(retract_turn(&["findings"]));
        assert!(!heads(&log).contains(&"findings".to_string()));
        // Re-defining the name brings it back (a later value→decl rebind).
        log.push(turn("findings = [1]", vec![val("findings")]));
        assert!(heads(&log).contains(&"findings".to_string()));
        let exports = cumulative_exports_before(&log, log.turns.len() + 1);
        assert!(exports.iter().any(|e| e.head_name() == "findings"));
        assert!(log
            .replayable_sources()
            .iter()
            .any(|s| s.contains("findings = [1]")));
    }

    #[test]
    fn retracting_absent_name_leaves_exports_unchanged() {
        let mut log = DeclLog::new();
        log.push(turn("keep t = t", vec![val("keep")]));
        let before = cumulative_exports_before(&log, log.turns.len() + 1);
        log.push(retract_turn(&["never_defined"]));
        let after = cumulative_exports_before(&log, log.turns.len() + 1);
        assert_eq!(before, after, "retracting an absent name is a no-op fold");
    }
}
