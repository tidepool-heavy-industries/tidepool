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

/// Ordered external import specifications for model-authored source.
///
/// Entries omit the leading `import`, matching Tidepool's template builders.
/// Extraction from authored declarations deliberately recognizes only the
/// established single-line import grammar. Actor program images use a
/// structured exact-export facade; this remains the final rendered source view
/// shared by existing harness and REPL compilation.
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

    /// Record the ordinary single-line imports present in declaration source.
    ///
    /// Declaration rendering and workbench persistence support the same
    /// practical import grammar: one complete `import ...` specification per
    /// line. The stored form omits the keyword so it can be fed back through
    /// turn templates without another representation change.
    pub fn extend_declaration_source(&mut self, source: &str) {
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

    /// Attach the exact same external import view to source that will be
    /// persisted in the declaration plane.
    #[must_use]
    pub fn declaration_source(&self, body: &str) -> String {
        let prefix = self.declaration_prefix();
        if prefix.is_empty() {
            body.to_string()
        } else {
            format!("{prefix}\n{body}")
        }
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
    session: SessionId,
    lexical_scope: ScopeId,
    root: PathBuf,
    persistent_imports: SourceImports,
    library: Option<SessionModule>,
    visible_values: Vec<SessionModule>,
    injected_values: Vec<SessionModule>,
    next_value_generation: Generation,
}

impl SessionCompileView {
    #[allow(
        clippy::too_many_arguments,
        reason = "the compile view is one immutable snapshot of these independently owned fields"
    )]
    pub(crate) fn new(
        session: SessionId,
        lexical_scope: ScopeId,
        root: PathBuf,
        persistent_imports: SourceImports,
        library: Option<SessionModule>,
        mut visible_values: Vec<SessionModule>,
        mut injected_values: Vec<SessionModule>,
        next_value_generation: Generation,
    ) -> Self {
        sort_modules(&mut visible_values);
        sort_modules(&mut injected_values);
        Self {
            session,
            lexical_scope,
            root,
            persistent_imports,
            library,
            visible_values,
            injected_values,
            next_value_generation,
        }
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

    /// External imports plus this scope's current declaration and value
    /// modules, ready for a turn template.
    #[must_use]
    pub fn turn_imports(&self, external: &SourceImports) -> String {
        let mut specs = self.workbench_imports(external);
        if let Some(module) = self.library {
            specs.extend_text(&module.module_name());
        }
        for module in &self.visible_values {
            specs.extend_text(&module.module_name());
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
        let view = SessionCompileView::new(
            SessionId(4),
            ScopeId::ROOT,
            PathBuf::from("/session"),
            SourceImports::from_specs(["Data.Set qualified as Set"]),
            Some(SessionModule::lib(Generation(3))),
            vec![SessionModule::val(Generation(5))],
            vec![
                SessionModule::val(Generation(2)),
                SessionModule::val(Generation(5)),
            ],
            Generation(6),
        );
        let external = SourceImports::from_specs(["HarnessTypes (Decision (..))"]);

        assert_eq!(
            view.turn_imports(&external),
            "HarnessTypes (Decision (..))\nData.Set qualified as Set\nTidepool.Session.Lib.G3\nTidepool.Session.Val.G5"
        );
        assert_eq!(
            view.injected_module_names(),
            ["Tidepool.Session.Val.G2", "Tidepool.Session.Val.G5"]
        );
    }

    #[test]
    fn source_imports_extract_and_render_deterministically() {
        let mut imports = SourceImports::from_specs(["Tidepool.Actors.Shoal"]);
        imports.extend_declaration_source(
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
