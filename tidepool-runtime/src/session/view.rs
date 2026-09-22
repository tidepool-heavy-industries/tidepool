//! Immutable source-side views of a resident Haskell session.
//!
//! Compilation must happen while the live machine is stowed in its registry,
//! so callers cannot borrow [`super::PersistentSession`] while invoking GHC.
//! This module is the copyable membrane between those phases: it snapshots
//! exact module identities and paths, but owns no machine, roots, compiler
//! cache, or declaration log.

use std::path::{Path, PathBuf};

use tidepool_codegen::scope::ScopeId;
use tidepool_repr::{Generation, SessionId, SessionModule};

/// Apply a frontend's explicit vocabulary replacements to generated imports.
/// Qualified imports remain available; authored imports are normalized by GHC.
#[must_use]
pub fn hide_preamble_exports(preamble: &str, exports: &[super::ExportItem]) -> String {
    let heads = exports.iter().collect::<Vec<_>>();
    preamble
        .split_inclusive('\n')
        .map(|line| {
            if line.trim_start().starts_with("import ") {
                let rewritten =
                    super::render::hide_session_heads(line.trim_end_matches('\n'), &heads);
                if line.ends_with('\n') {
                    format!("{rewritten}\n")
                } else {
                    rewritten
                }
            } else {
                line.to_owned()
            }
        })
        .collect()
}

/// Ordered external import specifications for model-authored source.
///
/// Entries omit the leading `import`, matching Tidepool's template builders.
/// Authored imports arrive normalized by GHC. Actor program images use a
/// structured exact-export facade; this is the rendered source view shared
/// by session frontends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceImports {
    specs: Vec<String>,
}

impl SourceImports {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn from_specs(specs: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut imports = Self::new();
        for spec in specs {
            imports.extend_text(spec.as_ref());
        }
        imports
    }

    /// Add newline-separated import specifications, preserving first-seen
    /// order and removing exact duplicates.
    pub fn extend_text(&mut self, text: &str) {
        for spec in text.lines().map(str::trim).filter(|spec| !spec.is_empty()) {
            if !self.specs.iter().any(|existing| existing == spec) {
                self.specs.push(spec.to_string());
            }
        }
    }

    /// Add another ordered import set, preserving the first occurrence of
    /// every exact specification.
    pub fn extend(&mut self, other: &Self) {
        for spec in &other.specs {
            self.extend_text(spec);
        }
    }

    /// Read imports from a generated preamble, whose renderer already owns
    /// the one-import-per-line format. Authored source must pass through GHC.
    pub fn extend_generated_imports(&mut self, source: &str) {
        for line in source.lines().map(str::trim) {
            let Some(rest) = line.strip_prefix("import") else {
                continue;
            };
            if rest.starts_with(char::is_whitespace) {
                self.extend_text(rest.trim_start());
            }
        }
    }

    #[must_use]
    pub fn specs(&self) -> &[String] {
        &self.specs
    }

    /// Render the persistent workbench view in familiar Haskell syntax.
    #[must_use]
    pub fn source_lines(&self) -> Vec<String> {
        self.specs
            .iter()
            .map(|spec| format!("import {spec}"))
            .collect()
    }

    /// Render for a turn-template import hole (without `import` keywords).
    #[must_use]
    pub fn template_text(&self) -> String {
        self.specs.join("\n")
    }

    /// Render as real source imports for a declaration module.
    #[must_use]
    pub fn declaration_prefix(&self) -> String {
        self.specs
            .iter()
            .map(|spec| format!("import {spec}\n"))
            .collect()
    }
}

/// Exact source-side state visible from one resident lexical scope.
///
/// `library` and `visible_values` are the modules a new turn imports.
/// `injected_values` is wider: it retains shadowed generations needed to load
/// already-compiled references. The distinction is represented here once so
/// harness, REPL, and actor workbenches cannot assemble subtly different
/// module sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCompileView {
    pub(super) session: SessionId,
    pub(super) lexical_scope: ScopeId,
    pub(super) root: PathBuf,
    pub(super) persistent_imports: SourceImports,
    pub(super) library: Option<SessionModule>,
    pub(super) visible_values: Vec<SessionModule>,
    /// Names visible from each value interface.  A generated interface can
    /// carry helper binders beside its published value; importing its whole
    /// module would accidentally expose those helpers before the value plane
    /// commits them.
    pub(super) visible_value_names: Vec<(SessionModule, Vec<String>)>,
    pub(super) injected_values: Vec<SessionModule>,
    pub(super) next_value_generation: Generation,
    pub(super) shadowing: Vec<super::ExportItem>,
    pub(super) staged_hiding: Vec<(SessionModule, Vec<super::ExportItem>)>,
}

impl SessionCompileView {
    pub(super) fn canonicalize(mut self) -> Self {
        sort_modules(&mut self.visible_values);
        self.visible_value_names
            .sort_by_key(|(module, _)| module.module_name());
        for (_, names) in &mut self.visible_value_names {
            names.sort();
            names.dedup();
        }
        sort_modules(&mut self.injected_values);
        self
    }

    /// Current scope bindings take precedence over implicit vocabulary imports,
    /// just as they do in persisted declaration modules.
    #[must_use]
    pub fn shadow_preamble(&self, preamble: &str) -> String {
        hide_preamble_exports(preamble, &self.shadowing)
    }

    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    #[must_use]
    pub fn lexical_scope(&self) -> ScopeId {
        self.lexical_scope
    }

    #[must_use]
    pub fn session_root(&self) -> &Path {
        &self.root
    }

    /// User-authored imports that persist at this lexical scope.
    #[must_use]
    pub fn persistent_imports(&self) -> &SourceImports {
        &self.persistent_imports
    }

    /// Frontend-provided imports followed by user-authored persistent imports.
    #[must_use]
    pub fn workbench_imports(&self, external: &SourceImports) -> SourceImports {
        let mut imports = external.clone();
        imports.extend(&self.persistent_imports);
        imports
    }

    #[must_use]
    pub fn library(&self) -> Option<SessionModule> {
        self.library
    }

    #[must_use]
    pub fn visible_values(&self) -> &[SessionModule] {
        &self.visible_values
    }

    #[must_use]
    pub fn injected_values(&self) -> &[SessionModule] {
        &self.injected_values
    }

    #[must_use]
    pub fn next_value_generation(&self) -> Generation {
        self.next_value_generation
    }

    /// Use a declaration module that has been validated for a cell but is not
    /// yet committed to the live declaration log.
    #[must_use]
    pub fn with_staged_library(
        mut self,
        module: SessionModule,
        declared: &[super::ExportItem],
    ) -> Self {
        self.hide_staged_names(declared);
        self.shadowing.extend_from_slice(declared);
        self.library = Some(module);
        self
    }

    /// Extend a preflight view with the thin value interface emitted by an
    /// earlier statement in the same cell.
    #[must_use]
    pub fn with_staged_values(
        mut self,
        module: SessionModule,
        names: impl IntoIterator<Item = String>,
    ) -> Self {
        let names = names
            .into_iter()
            .map(|name| super::ExportItem::Value { name })
            .collect::<Vec<_>>();
        let visible_names = names
            .iter()
            .filter_map(|item| match item {
                super::ExportItem::Value { name } => Some(name.clone()),
                _ => None,
            })
            .collect();
        self.hide_staged_names(&names);
        self.shadowing.extend(names);
        self.visible_values.push(module);
        self.visible_value_names.push((module, visible_names));
        self.injected_values.push(module);
        self.next_value_generation = module.gen().next();
        self.canonicalize()
    }

    // Shadow names in earlier staged interfaces without dropping other names
    // exported by those modules or losing their qualified identity.
    fn hide_staged_names(&mut self, names: &[super::ExportItem]) {
        for module in self.library.iter().chain(self.visible_values.iter()) {
            if let Some((_, hidden)) = self.staged_hiding.iter_mut().find(|(key, _)| key == module)
            {
                hidden.extend_from_slice(names);
            } else {
                self.staged_hiding.push((*module, names.to_vec()));
            }
        }
    }

    fn staged_import(&self, module: SessionModule) -> String {
        let hidden = self
            .staged_hiding
            .iter()
            .find(|(key, _)| *key == module)
            .map(|(_, hidden)| hidden.iter().collect::<Vec<_>>())
            .unwrap_or_default();
        let name = module.module_name();
        let unqualified = super::render::hide_session_heads(&name, &hidden);
        if hidden.is_empty() {
            unqualified
        } else {
            format!("{unqualified}\nqualified {name}")
        }
    }

    fn visible_value_import(&self, module: SessionModule) -> String {
        let Some((_, names)) = self
            .visible_value_names
            .iter()
            .find(|(candidate, _)| *candidate == module)
        else {
            return self.staged_import(module);
        };
        let hidden = self
            .staged_hiding
            .iter()
            .find(|(key, _)| *key == module)
            .map(|(_, hidden)| hidden.iter().collect::<Vec<_>>())
            .unwrap_or_default();
        let names = names
            .iter()
            .filter(|name| {
                !hidden.iter().any(|item| {
                    matches!(item, super::ExportItem::Value { name: hidden } if hidden == *name)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        let module_name = module.module_name();
        if names.is_empty() {
            return format!("qualified {module_name}");
        }
        let unqualified = format!("{module_name} ({})", names.join(", "));
        if hidden.is_empty() {
            unqualified
        } else {
            format!("{unqualified}\nqualified {module_name}")
        }
    }

    /// External imports plus this scope's current declaration and value
    /// modules, ready for a turn template.
    #[must_use]
    pub fn turn_imports(&self, external: &SourceImports) -> String {
        let mut specs = SourceImports::new();
        specs.extend_generated_imports(
            &self.shadow_preamble(&self.workbench_imports(external).declaration_prefix()),
        );
        if let Some(module) = self.library {
            specs.extend_text(&self.staged_import(module));
        }
        for module in &self.visible_values {
            specs.extend_text(&self.visible_value_import(*module));
        }
        specs.template_text()
    }

    /// Module names passed to the extractor's value-iface injection lane.
    #[must_use]
    pub fn injected_module_names(&self) -> Vec<String> {
        self.injected_values
            .iter()
            .map(SessionModule::module_name)
            .collect()
    }

    /// Base includes plus this session's exact module tree.
    #[must_use]
    pub fn include_paths(&self, base: &[PathBuf]) -> Vec<PathBuf> {
        let mut include = base.to_vec();
        if !include.iter().any(|path| path == &self.root) {
            include.push(self.root.clone());
        }
        include
    }
}

fn sort_modules(modules: &mut Vec<SessionModule>) {
    modules.sort_by_key(SessionModule::module_name);
    modules.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_view_keeps_visible_and_injected_value_sets_distinct() {
        let mut view = SessionCompileView {
            session: SessionId(4),
            lexical_scope: ScopeId::ROOT,
            root: PathBuf::from("/session"),
            persistent_imports: SourceImports::from_specs(["Data.Set qualified as Set"]),
            library: Some(SessionModule::lib(Generation(3))),
            visible_values: vec![SessionModule::val(Generation(5))],
            visible_value_names: Vec::new(),
            injected_values: vec![
                SessionModule::val(Generation(2)),
                SessionModule::val(Generation(5)),
            ],
            next_value_generation: Generation(6),
            shadowing: Vec::new(),
            staged_hiding: Vec::new(),
        }
        .canonicalize();
        let external = SourceImports::from_specs(["HarnessTypes (Decision (..))"]);

        assert_eq!(
            view.turn_imports(&external),
            "HarnessTypes (Decision (..))\nData.Set qualified as Set\nTidepool.Session.Lib.G3\nTidepool.Session.Val.G5"
        );
        assert_eq!(
            view.injected_module_names(),
            ["Tidepool.Session.Val.G2", "Tidepool.Session.Val.G5"]
        );
        view.shadowing.push(super::super::ExportItem::Value {
            name: "after".into(),
        });
        let external = SourceImports::from_specs(["Tidepool.Duration (after)"]);
        assert_eq!(view.turn_imports(&external),
            "Tidepool.Duration ()\nData.Set qualified as Set\nTidepool.Session.Lib.G3\nTidepool.Session.Val.G5");
    }

    #[test]
    fn staged_values_shadow_names_without_losing_old_module_identity() {
        let view = SessionCompileView {
            session: SessionId(4),
            lexical_scope: ScopeId::ROOT,
            root: PathBuf::from("/session"),
            persistent_imports: SourceImports::default(),
            library: None,
            visible_values: vec![SessionModule::val(Generation(5))],
            visible_value_names: Vec::new(),
            injected_values: vec![SessionModule::val(Generation(5))],
            next_value_generation: Generation(6),
            shadowing: Vec::new(),
            staged_hiding: Vec::new(),
        }
        .with_staged_values(SessionModule::val(Generation(6)), ["answer".into()])
        .with_staged_values(SessionModule::val(Generation(7)), ["answer".into()]);

        assert_eq!(view.turn_imports(&SourceImports::default()),
            "Tidepool.Session.Val.G5 hiding (answer)\nqualified Tidepool.Session.Val.G5\nTidepool.Session.Val.G6 hiding (answer)\nqualified Tidepool.Session.Val.G6\nTidepool.Session.Val.G7");
        assert_eq!(
            view.injected_module_names(),
            [
                "Tidepool.Session.Val.G5",
                "Tidepool.Session.Val.G6",
                "Tidepool.Session.Val.G7"
            ]
        );
        assert_eq!(view.next_value_generation(), Generation(8));
    }

    #[test]
    fn source_imports_extract_and_render_deterministically() {
        let mut imports = SourceImports::from_specs(["Tidepool.Actors.Shoal"]);
        imports.extend_generated_imports(
            "import qualified Data.Set as Set\nimport Tidepool.Actors.Shoal\nvalue = Set.empty",
        );

        assert_eq!(
            imports.source_lines(),
            [
                "import Tidepool.Actors.Shoal",
                "import qualified Data.Set as Set"
            ]
        );
    }
}
