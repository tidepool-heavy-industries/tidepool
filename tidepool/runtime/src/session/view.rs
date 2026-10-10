//! Immutable source-side views of a resident Haskell session.
//!
//! Compilation must happen while the live machine is stowed in its registry,
//! so callers cannot borrow [`super::PersistentSession`] while invoking GHC.
//! This module is the copyable membrane between those phases: it snapshots
//! exact module identities and paths, but owns no machine, roots, compiler
//! cache, or declaration log.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidepool_codegen::scope::ScopeId;
use tidepool_repr::{Generation, SessionId, SessionModule};
use tidepool_toolchain::declaration_join::ExactDeclarationContext;

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

/// A source type expression and the imports issued with its nominal names.
/// This supplies compiler source, not effect or resource authority. Authored
/// expressions may have no issued imports and must resolve in their source scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HaskellTypeSource {
    expression: String,
    required_imports: SourceImports,
}

impl HaskellTypeSource {
    pub fn new(expression: impl Into<String>, required_imports: SourceImports) -> Self {
        Self {
            expression: expression.into(),
            required_imports,
        }
    }

    pub fn expression(&self) -> &str {
        &self.expression
    }

    pub fn required_imports(&self) -> &SourceImports {
        &self.required_imports
    }

    /// Assemble issued type imports with the consuming source's imports once.
    pub fn source_imports(&self, imports: &SourceImports) -> SourceImports {
        let mut imports = imports.clone();
        imports.extend(&self.required_imports);
        imports
    }
}

impl From<String> for HaskellTypeSource {
    fn from(expression: String) -> Self {
        Self::new(expression, SourceImports::default())
    }
}

impl From<&str> for HaskellTypeSource {
    fn from(expression: &str) -> Self {
        expression.to_owned().into()
    }
}

impl std::fmt::Display for HaskellTypeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.expression())
    }
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
    pub(super) injected_values: Vec<SessionModule>,
    pub(super) next_value_generation: Generation,
    pub(super) projection: Arc<CompileViewProjection>,
    pub(super) request_context:
        Option<Arc<tidepool_toolchain::declaration_join::ExactCompileContext>>,
}

/// The reserved declaration owner and its actual compiler import are one
/// value, so a local original cannot masquerade as a cumulative interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CompileLibrary {
    Source(SessionModule),
    Certified {
        original: SessionModule,
        projection: Arc<super::CertifiedDeclarationProjection>,
    },
    Recovered {
        original: SessionModule,
        evidence: Arc<tidepool_toolchain::declaration_join::RecoveredDeclarationTip>,
    },
}
impl CompileLibrary {
    fn original(&self) -> SessionModule {
        match self {
            Self::Source(module) => *module,
            Self::Certified { original, .. } | Self::Recovered { original, .. } => *original,
        }
    }
    fn import_name(&self) -> String {
        match self {
            Self::Source(module) => module.module_name(),
            Self::Certified { projection, .. } => projection.module_name().to_owned(),
            Self::Recovered { evidence, .. } => evidence.root().module.clone(),
        }
    }
}

/// Immutable lexical metadata shared by readers of the same exact view.
/// Request identity, generation and injection selection remain on the view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CompileViewProjection {
    pub(super) root: PathBuf,
    pub(super) persistent_imports: SourceImports,
    pub(super) library: Option<CompileLibrary>,
    pub(super) visible_values: Vec<SessionModule>,
    /// Names visible from each value interface.  A generated interface can
    /// carry helper binders beside its published value; importing its whole
    /// module would accidentally expose those helpers before the binding store
    /// commits them.
    pub(super) visible_value_names: Vec<(SessionModule, Vec<String>)>,
    /// The part of `injected_values` a turn compiled at `lexical_scope` can
    /// actually reach: the value modules owned by the frames its lookup reads
    /// and by its inherited tip. Every other live module is injected only so
    /// the whole session stays findable; another actor's binds and releases
    /// move it without touching what this turn compiled against.
    ///
    /// An inherited declaration module can import an ancestor's value
    /// generation that the ancestor has since shadowed; that module is not
    /// in this set. Leaving it out is safe only because an ancestor scope
    /// outlives its descendants, so the ancestor's frame keeps it live for as
    /// long as this scope exists.
    pub(super) reachable_values: Vec<SessionModule>,
    pub(super) shadowing: Vec<super::ExportItem>,
    pub(super) staged_hiding: Vec<(SessionModule, Vec<super::ExportItem>)>,
    /// Immutable original declaration products and selected lexical graph.
    /// This is separate from the include roots used to discover fresh source.
    pub(super) exact_context: Option<Arc<ExactDeclarationContext>>,
}

impl SessionCompileView {
    /// Canonical runtime view identity; rendered diagnostic observations never
    /// participate in compiler admission authority.
    #[cfg(test)]
    pub(super) fn admission_digest(&self) -> [u8; 32] {
        self.admission_commitment().0
    }

    pub(super) fn admission_commitment(&self) -> ([u8; 32], usize) {
        let items = |items: &[super::ExportItem]| {
            items.iter().map(|item| {
            serde_json::json!({"kind": match item { super::ExportItem::Value { .. } => "value", super::ExportItem::Type { .. } => "type", super::ExportItem::Class { .. } => "class" },
                "head": item.head_name(), "names": item.all_names().collect::<Vec<_>>()})
        }).collect::<Vec<_>>()
        };
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": "runtime-compile-view-v1", "session": self.session.0,
            "scope": self.lexical_scope.0, "imports": self.projection.persistent_imports.specs(),
            "library": self.projection.library.as_ref().map(CompileLibrary::import_name),
            "visible": self.projection.visible_value_names.iter().map(|(module, names)| (module.module_name(), names)).collect::<Vec<_>>(),
            "reachable": self.projection.reachable_values.iter().map(SessionModule::module_name).collect::<Vec<_>>(),
            "shadowing": items(&self.projection.shadowing),
            "hiding": self.projection.staged_hiding.iter().map(|(module, hidden)| (module.module_name(), items(hidden))).collect::<Vec<_>>(),
            "exact": self.projection.exact_context.as_ref().map(|context| context.semantic_sha256()),
        })).expect("runtime view contains serializable identities");
        (*blake3::hash(&bytes).as_bytes(), bytes.len())
    }

    pub(super) fn canonicalize(mut self) -> Self {
        let projection = Arc::make_mut(&mut self.projection);
        sort_modules(&mut projection.visible_values);
        projection
            .visible_value_names
            .sort_by_key(|(module, _)| module.module_name());
        for (_, names) in &mut projection.visible_value_names {
            names.sort();
            names.dedup();
        }
        sort_modules(&mut self.injected_values);
        sort_modules(&mut projection.reachable_values);
        self
    }

    /// Refresh only request-local inventory without copying the cached view's
    /// old inventory. The lexical projection and its authority stay fixed.
    #[must_use]
    pub(super) fn with_request_inventory(
        &self,
        mut injected_values: Vec<SessionModule>,
        next_value_generation: Generation,
    ) -> Self {
        sort_modules(&mut injected_values);
        Self {
            session: self.session,
            lexical_scope: self.lexical_scope,
            injected_values,
            next_value_generation,
            projection: self.projection.clone(),
            request_context: self.request_context.clone(),
        }
    }

    /// Keep selected live values injected for already-compiled references,
    /// while withholding their unqualified exports from a new source turn.
    #[must_use]
    pub fn hide_value_names(mut self, names: &[String]) -> Self {
        for (_, published) in &mut Arc::make_mut(&mut self.projection).visible_value_names {
            published.retain(|name| !names.contains(name));
        }
        self
    }

    /// Current scope bindings take precedence over implicit vocabulary imports,
    /// just as they do in persisted declaration modules.
    #[must_use]
    pub fn shadow_preamble(&self, preamble: &str) -> String {
        hide_preamble_exports(preamble, &self.projection.shadowing)
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
        &self.projection.root
    }

    /// User-authored imports that persist at this lexical scope.
    #[must_use]
    pub fn persistent_imports(&self) -> &SourceImports {
        &self.projection.persistent_imports
    }

    /// Frontend-provided imports followed by user-authored persistent imports.
    #[must_use]
    pub fn workbench_imports(&self, external: &SourceImports) -> SourceImports {
        let mut imports = external.clone();
        imports.extend(&self.projection.persistent_imports);
        imports
    }

    #[must_use]
    pub fn library(&self) -> Option<SessionModule> {
        self.projection
            .library
            .as_ref()
            .map(CompileLibrary::original)
    }

    /// The actual lexical interface imported by fresh source. This may differ
    /// from the reserved native declaration identity returned by library().
    #[must_use]
    pub fn library_import_module(&self) -> Option<String> {
        self.projection
            .library
            .as_ref()
            .map(CompileLibrary::import_name)
    }

    #[must_use]
    pub fn exact_declaration_context(&self) -> Option<&Arc<ExactDeclarationContext>> {
        self.projection.exact_context.as_ref()
    }

    /// Apply the exact same proof-bearing inputs used by runtime admission.
    /// Only the issued projection's selected lexical graph becomes visible.
    pub fn with_compile_inputs(
        mut self,
        inputs: &super::prepared::RuntimeCompileInputs,
    ) -> Result<Self, crate::CompileError> {
        if !inputs.projections().is_empty() {
            let mut context = match &self.projection.exact_context {
                Some(context) => (**context).clone(),
                None => ExactDeclarationContext::new(&[], &[], Vec::new())?,
            };
            let mut lexical = Vec::new();
            let mut joins = Vec::new();
            for projection in inputs.projections() {
                lexical.extend_from_slice(projection.context().lexical_graph());
                joins.push(projection.receipt().clone());
            }
            context = context.extend_lexical_joins(&joins, &lexical)?;
            Arc::make_mut(&mut self.projection).exact_context = Some(Arc::new(context));
        }
        if let Some(annotations) = inputs.annotations() {
            self = self.with_request_annotations(annotations)?;
        } else if !inputs.projections().is_empty() && self.request_context.is_some() {
            return Err(crate::CompileError::ExtractFailed(
                "certified projections must be applied before request annotations".into(),
            ));
        }
        Ok(self)
    }

    /// Request annotations and retained type interfaces are local to this
    /// compile view; the cached lexical projection remains unchanged.
    pub fn with_request_type_evidence(
        mut self,
        evidence: &super::SiteTypeEvidence,
    ) -> Result<Self, crate::CompileError> {
        self.request_context =
            Some(evidence.compile_context(self.projection.exact_context.as_ref())?);
        Ok(self)
    }

    /// Reconstruct the complete request-local compiler authority atomically.
    pub fn with_request_annotations(
        self,
        annotations: &super::RequestCompileAnnotations,
    ) -> Result<Self, crate::CompileError> {
        self.with_request_type_evidence(annotations.evidence())?
            .with_request_helper_recipe(annotations.helper_recipe())
    }

    /// Bind helpers only to the authenticated request context in this view.
    pub fn with_request_helper_recipe(
        mut self,
        recipe: tidepool_toolchain::declaration_join::RequestHelperRecipe,
    ) -> Result<Self, crate::CompileError> {
        match self.request_context.take() {
            Some(context) => {
                self.request_context = Some(Arc::new(
                    (*context).clone().with_request_helper_recipe(recipe)?,
                ));
            }
            None if recipe == tidepool_toolchain::declaration_join::RequestHelperRecipe::None => {}
            None => {
                return Err(crate::CompileError::ExtractFailed(
                    "actor reply helpers require an authenticated request context".into(),
                ));
            }
        }
        Ok(self)
    }

    #[must_use]
    pub fn exact_compile_context(
        &self,
    ) -> Option<Arc<tidepool_toolchain::declaration_join::ExactCompileContext>> {
        self.request_context.clone().or_else(|| {
            self.projection.exact_context.as_ref().map(|declarations| {
                Arc::new(
                    tidepool_toolchain::declaration_join::ExactCompileContext::new(
                        declarations.clone(),
                    ),
                )
            })
        })
    }

    #[must_use]
    pub fn visible_values(&self) -> &[SessionModule] {
        &self.projection.visible_values
    }

    #[must_use]
    pub fn injected_values(&self) -> &[SessionModule] {
        &self.injected_values
    }

    /// A protected compiler offer may read only the exact lexical dependency
    /// closure. Other scopes' live modules do not become compiler inputs.
    #[must_use]
    pub fn with_scoped_injection(mut self) -> Self {
        self.injected_values
            .clone_from(&self.projection.reachable_values);
        self
    }

    /// The injected value modules a turn compiled at this scope can reach;
    /// see the field's documentation for what is outside it.
    #[must_use]
    pub fn reachable_values(&self) -> &[SessionModule] {
        &self.projection.reachable_values
    }

    #[must_use]
    pub fn next_value_generation(&self) -> Generation {
        self.next_value_generation
    }

    /// Whether a turn compiled against `compiled_against` may still be
    /// installed now that the session presents `self` for the same scope.
    ///
    /// Everything the turn imported must be identical: session identity,
    /// scope, lexical root, imports, the declaration module, the visible
    /// value modules and names, and shadowing. Of the injected value
    /// modules, only the ones the turn could reach
    /// (`compiled_against.reachable_values()`) must still be live; the rest of
    /// the session's live set is injected for findability alone, so another
    /// actor binding or releasing its own values cannot invalidate this turn.
    /// A newly injected module the turn did not import is likewise harmless.
    ///
    /// `next_value_generation` is excluded: a caller that reserved its own
    /// generation before releasing its checkout (`ResidentSession::
    /// reserve_value_generations_through`) expects that counter to have
    /// moved on by the time it re-derives a view to install against.
    #[must_use]
    pub fn is_current_for(&self, compiled_against: &Self) -> bool {
        self.session == compiled_against.session
            && self.lexical_scope == compiled_against.lexical_scope
            && self.projection.root == compiled_against.projection.root
            && self.projection.persistent_imports == compiled_against.projection.persistent_imports
            && self.projection.library == compiled_against.projection.library
            && self.projection.visible_values == compiled_against.projection.visible_values
            && self.projection.visible_value_names
                == compiled_against.projection.visible_value_names
            && self.projection.shadowing == compiled_against.projection.shadowing
            && self.projection.staged_hiding == compiled_against.projection.staged_hiding
            && match (
                &self.projection.exact_context,
                &compiled_against.projection.exact_context,
            ) {
                (Some(current), Some(compiled)) => {
                    Arc::ptr_eq(current, compiled)
                        || current.semantic_sha256() == compiled.semantic_sha256()
                }
                (None, None) => true,
                _ => false,
            }
            && compiled_against
                .projection
                .reachable_values
                .iter()
                .all(|module| self.injected_values.contains(module))
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
        let projection = Arc::make_mut(&mut self.projection);
        projection.shadowing.extend_from_slice(declared);
        projection.library = Some(CompileLibrary::Source(module));
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
        let projection = Arc::make_mut(&mut self.projection);
        projection.shadowing.extend(names);
        projection.visible_values.push(module);
        projection.visible_value_names.push((module, visible_names));
        self.injected_values.push(module);
        projection.reachable_values.push(module);
        self.next_value_generation = module.gen().next();
        self.canonicalize()
    }

    // Shadow names in earlier staged interfaces without dropping other names
    // exported by those modules or losing their qualified identity.
    fn hide_staged_names(&mut self, names: &[super::ExportItem]) {
        let projection = Arc::make_mut(&mut self.projection);
        for module in projection
            .library
            .iter()
            .map(CompileLibrary::original)
            .chain(projection.visible_values.iter().copied())
        {
            if let Some((_, hidden)) = projection
                .staged_hiding
                .iter_mut()
                .find(|(key, _)| *key == module)
            {
                hidden.extend_from_slice(names);
            } else {
                projection.staged_hiding.push((module, names.to_vec()));
            }
        }
    }

    fn staged_import(&self, module: SessionModule) -> String {
        let hidden = self
            .projection
            .staged_hiding
            .iter()
            .find(|(key, _)| *key == module)
            .map(|(_, hidden)| hidden.iter().collect::<Vec<_>>())
            .unwrap_or_default();
        let name = self
            .projection
            .library
            .as_ref()
            .filter(|library| library.original() == module)
            .map(CompileLibrary::import_name)
            .unwrap_or_else(|| module.module_name());
        let unqualified = super::render::hide_session_heads(&name, &hidden);
        if hidden.is_empty() {
            unqualified
        } else {
            format!("{unqualified}\nqualified {name}")
        }
    }

    fn visible_value_import(&self, module: SessionModule) -> String {
        let Some((_, published)) = self
            .projection
            .visible_value_names
            .iter()
            .find(|(candidate, _)| *candidate == module)
        else {
            return self.staged_import(module);
        };
        let hidden = self
            .projection
            .staged_hiding
            .iter()
            .find(|(key, _)| *key == module)
            .map(|(_, hidden)| hidden.iter().collect::<Vec<_>>())
            .unwrap_or_default();
        let is_hidden = |candidate: &String| {
            hidden
                .iter()
                .any(|item| matches!(item, super::ExportItem::Value { name } if name == candidate))
        };
        let visible = published
            .iter()
            .filter(|name| !is_hidden(name))
            .cloned()
            .collect::<Vec<_>>();
        let shadowed = published
            .iter()
            .filter(|name| is_hidden(name))
            .cloned()
            .collect::<Vec<_>>();
        let module_name = module.module_name();
        let render_names = |names: &[String]| {
            names
                .iter()
                .map(|name| super::ExportItem::Value { name: name.clone() }.render_entry())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut imports = Vec::with_capacity(2);
        if !visible.is_empty() {
            imports.push(format!("{module_name} ({})", render_names(&visible)));
        }
        if !shadowed.is_empty() {
            imports.push(format!(
                "qualified {module_name} ({})",
                render_names(&shadowed)
            ));
        }
        imports.join("\n")
    }

    /// External imports plus this scope's current declaration and value
    /// modules, ready for a turn template.
    #[must_use]
    pub fn turn_imports(&self, external: &SourceImports) -> String {
        let mut specs = SourceImports::new();
        specs.extend_generated_imports(
            &self.shadow_preamble(&self.workbench_imports(external).declaration_prefix()),
        );
        if let Some(library) = &self.projection.library {
            specs.extend_text(&self.staged_import(library.original()));
        }
        for module in &self.projection.visible_values {
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
        if !include.iter().any(|path| path == &self.projection.root) {
            include.push(self.projection.root.clone());
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
            injected_values: vec![
                SessionModule::val(Generation(2)),
                SessionModule::val(Generation(5)),
            ],
            next_value_generation: Generation(6),
            request_context: None,
            projection: std::sync::Arc::new(crate::session::view::CompileViewProjection {
                root: PathBuf::from("/session"),
                persistent_imports: SourceImports::from_specs(["Data.Set qualified as Set"]),
                library: Some(CompileLibrary::Source(SessionModule::lib(Generation(3)))),
                visible_values: vec![SessionModule::val(Generation(5))],
                visible_value_names: Vec::new(),
                reachable_values: Vec::new(),
                shadowing: Vec::new(),
                staged_hiding: Vec::new(),
                exact_context: None,
            }),
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
        Arc::make_mut(&mut view.projection)
            .shadowing
            .push(super::super::ExportItem::Value {
                name: "after".into(),
            });
        let external = SourceImports::from_specs(["Tidepool.Duration (after)"]);
        assert_eq!(view.turn_imports(&external),
            "Tidepool.Duration ()\nData.Set qualified as Set\nTidepool.Session.Lib.G3\nTidepool.Session.Val.G5");
    }

    #[test]
    fn is_current_for_ignores_other_scopes_values_but_not_imports_or_reachable_modules() {
        let base = SessionCompileView {
            session: SessionId(4),
            lexical_scope: ScopeId::ROOT,
            injected_values: vec![
                SessionModule::val(Generation(2)),
                SessionModule::val(Generation(4)),
                SessionModule::val(Generation(5)),
            ],
            next_value_generation: Generation(6),
            request_context: None,
            projection: std::sync::Arc::new(crate::session::view::CompileViewProjection {
                root: PathBuf::from("/session"),
                persistent_imports: SourceImports::from_specs(["Data.Set qualified as Set"]),
                library: Some(CompileLibrary::Source(SessionModule::lib(Generation(3)))),
                visible_values: vec![SessionModule::val(Generation(5))],
                visible_value_names: Vec::new(),
                reachable_values: vec![
                    SessionModule::val(Generation(2)),
                    SessionModule::val(Generation(5)),
                ],
                shadowing: Vec::new(),
                staged_hiding: Vec::new(),
                exact_context: None,
            }),
        }
        .canonicalize();

        // Only the reserved generation moved forward: a caller that reserved
        // its own generation before releasing its checkout must not see this
        // as staleness.
        let mut reserved_further = base.clone();
        reserved_further.next_value_generation = Generation(9);
        assert!(reserved_further.is_current_for(&base));

        // Another scope bound G7 and released G4: neither was reachable from
        // this turn, so the compile still stands.
        let mut other_scope_moved = base.clone();
        other_scope_moved
            .injected_values
            .retain(|module| *module != SessionModule::val(Generation(4)));
        other_scope_moved
            .injected_values
            .push(SessionModule::val(Generation(7)));
        assert!(other_scope_moved.is_current_for(&base));

        // A reachable module (here a shadowed generation of this scope) that
        // is no longer live must be caught.
        let mut reachable_released = base.clone();
        reachable_released
            .injected_values
            .retain(|module| *module != SessionModule::val(Generation(2)));
        assert!(!reachable_released.is_current_for(&base));

        // A concurrent write that changes what the turn imported must be
        // caught.
        let mut visible_changed = base.clone();
        Arc::make_mut(&mut visible_changed.projection)
            .visible_values
            .push(SessionModule::val(Generation(7)));
        assert!(!visible_changed.is_current_for(&base));

        let mut shadowing_changed = base.clone();
        Arc::make_mut(&mut shadowing_changed.projection)
            .shadowing
            .push(super::super::ExportItem::Value {
                name: "interloper".into(),
            });
        assert!(!shadowing_changed.is_current_for(&base));

        let mut library_changed = base.clone();
        Arc::make_mut(&mut library_changed.projection).library =
            Some(CompileLibrary::Source(SessionModule::lib(Generation(8))));
        assert!(!library_changed.is_current_for(&base));
    }

    #[test]
    fn staged_values_shadow_names_without_losing_old_module_identity() {
        let view = SessionCompileView {
            session: SessionId(4),
            lexical_scope: ScopeId::ROOT,
            injected_values: vec![SessionModule::val(Generation(5))],
            next_value_generation: Generation(6),
            request_context: None,
            projection: std::sync::Arc::new(crate::session::view::CompileViewProjection {
                root: PathBuf::from("/session"),
                persistent_imports: SourceImports::default(),
                library: None,
                visible_values: vec![SessionModule::val(Generation(5))],
                visible_value_names: Vec::new(),
                reachable_values: Vec::new(),
                shadowing: Vec::new(),
                staged_hiding: Vec::new(),
                exact_context: None,
            }),
        }
        .with_staged_values(SessionModule::val(Generation(6)), ["answer".into()])
        .with_staged_values(SessionModule::val(Generation(7)), ["answer".into()]);

        assert_eq!(view.turn_imports(&SourceImports::default()),
            "Tidepool.Session.Val.G5 hiding (answer)\nqualified Tidepool.Session.Val.G5\nqualified Tidepool.Session.Val.G6 (answer)\nTidepool.Session.Val.G7 (answer)");
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
    fn exact_value_imports_never_expose_unpublished_generated_helpers() {
        let old = SessionModule::val(Generation(5));
        let view = SessionCompileView {
            session: SessionId(4),
            lexical_scope: ScopeId::ROOT,
            injected_values: vec![old],
            next_value_generation: Generation(6),
            request_context: None,
            projection: std::sync::Arc::new(crate::session::view::CompileViewProjection {
                root: PathBuf::from("/session"),
                persistent_imports: SourceImports::default(),
                library: None,
                visible_values: vec![old],
                visible_value_names: vec![(
                    old,
                    vec!["retained".into(), "alias".into(), ".+".into()],
                )],
                reachable_values: Vec::new(),
                shadowing: Vec::new(),
                staged_hiding: Vec::new(),
                exact_context: None,
            }),
        }
        .with_staged_values(SessionModule::val(Generation(6)), ["alias".into()]);

        let imports = view.turn_imports(&SourceImports::default());
        assert_eq!(
            imports,
            "Tidepool.Session.Val.G5 ((.+), retained)\n\
             qualified Tidepool.Session.Val.G5 (alias)\n\
             Tidepool.Session.Val.G6 (alias)"
        );
        assert!(!imports.contains("unpublishedHelper"));
    }

    #[test]
    fn source_imports_extract_and_render_deterministically() {
        let mut imports = SourceImports::from_specs(["Tidepool.Actors.Exomonad"]);
        imports.extend_generated_imports(
            "import qualified Data.Set as Set\nimport Tidepool.Actors.Exomonad\nvalue = Set.empty",
        );

        assert_eq!(
            imports.source_lines(),
            [
                "import Tidepool.Actors.Exomonad",
                "import qualified Data.Set as Set"
            ]
        );
    }
}
