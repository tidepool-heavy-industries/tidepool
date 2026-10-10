//! Source discovery and installation origin for an actor's spec.
//!
//! Source-defined specs report their discovery rule, searched roots and entry.
//! Explicit live specs retain compiled values and cannot be replaced by a
//! filesystem reload.

use std::path::{Path, PathBuf};

pub(crate) mod preparation;

/// The two names the whole convention consists of: the module, and the value
/// it exports.
pub(crate) const SPEC_MODULE: &str = "AgentSpec";
pub(crate) const SPEC_VALUE: &str = "agentSpec";

/// Which install rule answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecRule {
    /// `AgentSpec.agentSpec`, found in the active include roots.
    RunModule,
    /// The workspace's `[haskell] spec` key, for a workspace that wants
    /// another name.
    WorkspaceSpec,
    /// Nothing named a spec, so the host selects its supported notebook policy.
    BuiltinDefault,
}

impl SpecRule {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::RunModule => "run module",
            Self::WorkspaceSpec => "workspace spec key",
            Self::BuiltinDefault => "built-in default",
        }
    }
}

/// What one compile installs, and which rule chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSpec {
    pub rule: SpecRule,
    /// The qualified Haskell value the install fragment applies. `None` under
    /// [`SpecRule::BuiltinDefault`]: the host selects the workbench or async policy.
    pub entry: Option<String>,
    /// The file the entry was read from, when discovery found one on disk.
    /// A configured entry point names a module, not a path, so it has none.
    pub file: Option<PathBuf>,
    /// The source roots that were searched for the spec module, in the order
    /// GHC would have taken them.
    pub searched: Vec<PathBuf>,
}

impl ResolvedSpec {
    /// Identity of every ordered root available to the source-defined installer.
    pub(crate) fn source_closure_revision(&self) -> Result<String, String> {
        tidepool_toolchain::cache::source_roots_identity(
            b"exomonad-agent-spec-ordered-source-closure-v1",
            &self.searched,
        )
        .map_err(|error| error.to_string())
    }
    /// Revision of the module that supplies this entry, using the same first
    /// matching include root GHC will read. Configured entries have no
    /// discovery file, so their module path must be resolved here too.
    pub(crate) fn source_revision(&self) -> Option<String> {
        let (module, _) = self.entry.as_deref()?.rsplit_once('.')?;
        let source = PathBuf::from(module.replace('.', "/")).with_extension("hs");
        self.searched
            .iter()
            .find(|root| root.join(&source).is_file())
            .and_then(|root| layer_revision(std::slice::from_ref(root)))
    }

    /// One line naming the rule and the file, for status and for a reload
    /// receipt.
    pub(crate) fn describe(&self) -> String {
        let entry = self.entry.as_deref().unwrap_or("(none)");
        match &self.file {
            // The file is read from a published revision under the run root.
            // A reader edits the copy in the run's authored source roots, so the
            // name they know is shown; `file` keeps the path that was read.
            Some(file) => format!(
                "rule={} entry={entry} file={}",
                self.rule.label(),
                file.file_name().map_or_else(
                    || file.display().to_string(),
                    |name| name.to_string_lossy().into_owned()
                )
            ),
            None => format!(
                "rule={} entry={entry} searched={}",
                self.rule.label(),
                self.searched.len()
            ),
        }
    }

    /// The module the reload must pull into its checked closure. A spec found
    /// by convention is not in `[haskell] modules`, so a reload that did not
    /// add it would let a spec that fails to compile surface later, at an
    /// unrelated call, instead of failing its own reload.
    pub(crate) fn checked_module(&self) -> Option<String> {
        let entry = self.entry.as_deref()?;
        let (module, _) = entry.rsplit_once('.')?;
        Some(module.to_owned())
    }
}

/// How the currently installed handlers were obtained. Filesystem reload is
/// valid only for a spec installed from a prepared source revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpecOrigin {
    SourcePrepared(ResolvedSpec),
    ExplicitLive,
}

impl SpecOrigin {
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::SourcePrepared(resolved) => resolved.describe(),
            Self::ExplicitLive => "origin=explicit live spec".into(),
        }
    }

    pub(crate) fn checked_module(&self) -> Option<String> {
        self.resolved()?.checked_module()
    }

    pub(crate) fn resolved(&self) -> Option<&ResolvedSpec> {
        match self {
            Self::SourcePrepared(resolved) => Some(resolved),
            Self::ExplicitLive => None,
        }
    }

    pub(crate) fn is_explicit(&self) -> bool {
        matches!(self, Self::ExplicitLive)
    }
}

/// Resolve one actor's spec, stopping at the first rule that answers.
///
/// `layer` is the exact include graph this actor compiles against, private
/// helper roots first and shared run roots after them. Helper roots may only
/// contain `SessionHelpers`, so a discovered `AgentSpec` comes from the run.
pub fn resolve(layer: Vec<PathBuf>, spec: Option<&str>) -> ResolvedSpec {
    let file = layer.iter().find_map(|root| {
        let candidate = root.join(format!("{SPEC_MODULE}.hs"));
        candidate.is_file().then_some(candidate)
    });
    let searched = layer;
    if let Some(file) = file {
        return ResolvedSpec {
            rule: SpecRule::RunModule,
            entry: Some(format!("{SPEC_MODULE}.{SPEC_VALUE}")),
            file: Some(file),
            searched,
        };
    }
    if let Some(entry) = spec {
        return ResolvedSpec {
            rule: SpecRule::WorkspaceSpec,
            entry: Some(entry.to_owned()),
            file: None,
            searched,
        };
    }
    ResolvedSpec {
        rule: SpecRule::BuiltinDefault,
        entry: None,
        file: None,
        searched,
    }
}

/// The revision a published source include root currently resolves to.
///
/// Carried from install, not tracked: a layer's include roots are
/// `<layer>/active/<index>`, `active` is a symlink to `revisions/<identity>`,
/// and the identity is the resolved directory's own name. Non-layer roots
/// answer `None`.
pub(crate) fn layer_revision(layer: &[PathBuf]) -> Option<String> {
    let root = layer.first()?;
    let resolved = std::fs::canonicalize(root).ok()?;
    let revision: &Path = resolved.parent()?;
    // Only a published revision has an identity to name. A root that is not
    // inside a layer would otherwise answer with an unrelated directory name.
    if revision.parent()?.file_name()?.to_str()? != "revisions" {
        return None;
    }
    Some(revision.file_name()?.to_str()?.to_owned())
}

/// The effect row and expression used to install one selected spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationExpression {
    /// The authored effects used as the `installSpec` type application.
    pub effect_row: tidepool_runtime::session::HaskellTypeSource,
    /// The exact workbench statement that installs the selected spec.
    pub expression: String,
}

impl InstallationExpression {
    #[must_use]
    pub fn dispatcher_effect_row(&self) -> tidepool_runtime::session::HaskellTypeSource {
        let mut imports = self.effect_row.required_imports().clone();
        imports.extend_text("qualified Tidepool.Effects.Core\nqualified Tidepool.Agent.Contract");
        tidepool_runtime::session::HaskellTypeSource::new(
            format!(
                "(Tidepool.Effects.Core.AgentTools ': Tidepool.Agent.Contract.SyncEffects {})",
                self.effect_row.expression(),
            ),
            imports,
        )
    }
}

/// Render the single source of truth for actor spec installation.
///
/// Workspace checks typecheck this expression against each launchable role's
/// row, and the resident workbench executes the same expression at admission.
#[must_use]
pub fn installation_expression(
    entry: &str,
    effects: &[crate::ActorEffectKey],
) -> InstallationExpression {
    let mut imports = tidepool_runtime::session::SourceImports::default();
    let effect_row = format!(
        "'[{}]",
        effects
            .iter()
            .map(|effect| {
                let module = match effect {
                    crate::ActorEffectKey::Replies => "Tidepool.Agent.Reply.Internal",
                    crate::ActorEffectKey::Watches => "Tidepool.Agent.Watch.Internal",
                    _ => "Tidepool.Effects.Core",
                };
                imports.extend_text(&format!("qualified {module}"));
                format!("{module}.{}", effect.haskell_name())
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    InstallationExpression {
        expression: format!("_ <- Tidepool.Agent.Contract.installSpec @({effect_row}) {entry}"),
        effect_row: tidepool_runtime::session::HaskellTypeSource::new(effect_row, imports),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_expression_renders_the_exact_effect_row_and_entry() {
        let installation = installation_expression(
            "AgentSpec.agentSpec",
            &[
                crate::ActorEffectKey::Commands,
                crate::ActorEffectKey::Journal,
            ],
        );
        assert_eq!(
            installation.effect_row.expression(),
            "'[Tidepool.Effects.Core.Commands, Tidepool.Effects.Core.Journal]"
        );
        assert_eq!(
            installation.expression,
            "_ <- Tidepool.Agent.Contract.installSpec @('[Tidepool.Effects.Core.Commands, Tidepool.Effects.Core.Journal]) AgentSpec.agentSpec"
        );
        assert_eq!(
            installation.dispatcher_effect_row().expression(),
            "(Tidepool.Effects.Core.AgentTools ': Tidepool.Agent.Contract.SyncEffects '[Tidepool.Effects.Core.Commands, Tidepool.Effects.Core.Journal])"
        );
    }

    /// Convention wins over the configured name, and names the file it was read
    /// from, so a model never has to guess which spec is live.
    #[test]
    fn a_spec_module_in_the_run_graph_answers_first() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("AgentSpec.hs"), "module AgentSpec where\n").unwrap();
        let resolved = resolve(
            vec![root.path().to_path_buf()],
            Some("Project.Spec.agentSpec"),
        );
        assert_eq!(resolved.rule, SpecRule::RunModule);
        assert_eq!(resolved.entry.as_deref(), Some("AgentSpec.agentSpec"));
        assert_eq!(resolved.file, Some(root.path().join("AgentSpec.hs")));
        assert_eq!(resolved.checked_module().as_deref(), Some("AgentSpec"));
    }

    /// A run graph with no conventional spec uses the configured name, or
    /// the empty built-in spec when no name was configured.
    #[test]
    fn a_run_without_a_spec_file_uses_configured_or_empty_spec() {
        let root = tempfile::tempdir().unwrap();
        let layer = vec![root.path().to_path_buf()];
        let with_spec = resolve(layer.clone(), Some("Project.Spec.agentSpec"));
        assert_eq!(with_spec.rule, SpecRule::WorkspaceSpec);
        assert_eq!(with_spec.entry.as_deref(), Some("Project.Spec.agentSpec"));

        let nothing = resolve(layer, None);
        assert_eq!(nothing.rule, SpecRule::BuiltinDefault);
        assert_eq!(nothing.entry, None);
        assert_eq!(nothing.checked_module(), None);
    }

    #[test]
    fn origin_distinguishes_prepared_source_from_explicit_live_specs() {
        let prepared =
            SpecOrigin::SourcePrepared(resolve(Vec::new(), Some("Project.Spec.agentSpec")));
        assert!(!prepared.is_explicit());
        assert_eq!(prepared.checked_module().as_deref(), Some("Project.Spec"));
        assert!(prepared.resolved().is_some());
        assert!(prepared.describe().contains("workspace spec key"));

        let explicit = SpecOrigin::ExplicitLive;
        assert!(explicit.is_explicit());
        assert_eq!(explicit.checked_module(), None);
        assert_eq!(explicit.resolved(), None);
        assert!(explicit.describe().contains("explicit live spec"));
    }

    /// With no include roots, the configured name still determines the rule.
    #[test]
    fn an_actor_without_a_layer_reports_the_configured_rule() {
        let resolved = resolve(Vec::new(), Some("Project.Spec.agentSpec"));
        assert_eq!(resolved.rule, SpecRule::WorkspaceSpec);
        assert!(resolved.searched.is_empty());
        assert!(resolved.describe().contains("workspace spec key"));
        assert_eq!(layer_revision(&[]), None);
    }

    #[test]
    fn configured_entry_names_the_run_revision_that_supplies_its_module() {
        let root = tempfile::tempdir().unwrap();
        let revision = root.path().join("revisions/revision-one/0");
        std::fs::create_dir_all(revision.join("Project")).unwrap();
        std::fs::write(
            revision.join("Project/Tools.hs"),
            "module Project.Tools where\n",
        )
        .unwrap();
        std::os::unix::fs::symlink("revisions/revision-one", root.path().join("active")).unwrap();
        let resolved = resolve(
            vec![root.path().join("active/0")],
            Some("Project.Tools.agentSpec"),
        );
        assert_eq!(resolved.source_revision().as_deref(), Some("revision-one"));
    }

    #[test]
    fn installer_revision_covers_instances_and_order_across_all_roots() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(
            first.path().join("AgentSpec.hs"),
            "module AgentSpec where\n",
        )
        .unwrap();
        std::fs::write(
            second.path().join("Instances.hs"),
            "module Instances where\n",
        )
        .unwrap();
        let roots = vec![first.path().to_path_buf(), second.path().to_path_buf()];
        let original = resolve(roots.clone(), None)
            .source_closure_revision()
            .unwrap();
        std::fs::write(
            second.path().join("Instances.hs"),
            "module Instances where\ninstance Eq Int\n",
        )
        .unwrap();
        let edited = resolve(roots.clone(), None)
            .source_closure_revision()
            .unwrap();
        assert_ne!(original, edited);
        let mut reversed = roots;
        reversed.reverse();
        assert_ne!(
            edited,
            resolve(reversed, None).source_closure_revision().unwrap()
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 96,
            ..proptest::test_runner::Config::default()
        })]
        #[test]
        fn nominal_effect_names_and_imports_stay_coupled(indices in proptest::collection::vec(0usize..5, 0..16)) {
            // Independent nominal oracle: preserve row order and duplicates;
            // imports form an exact set, including defining modules of reexports.
            let oracle = [
                (crate::ActorEffectKey::Replies, "Tidepool.Agent.Reply.Internal", "Replies"),
                (crate::ActorEffectKey::Watches, "Tidepool.Agent.Watch.Internal", "Watches"),
                (crate::ActorEffectKey::Commands, "Tidepool.Effects.Core", "Commands"),
                (crate::ActorEffectKey::Journal, "Tidepool.Effects.Core", "Journal"),
                (crate::ActorEffectKey::AgentLaunch, "Tidepool.Effects.Core", "AgentLaunch"),
            ];
            let keys = indices.iter().map(|index| oracle[*index].0).collect::<Vec<_>>();
            let expected = format!("'[{}]", indices.iter().map(|index| {
                let (_, module, name) = oracle[*index]; format!("{module}.{name}")
            }).collect::<Vec<_>>().join(", "));
            let imports = indices.iter().map(|index| format!("qualified {}", oracle[*index].1))
                .collect::<std::collections::BTreeSet<_>>();
            let installation = installation_expression("Fixture.agentSpec", &keys);
            proptest::prop_assert_eq!(installation.effect_row.expression(), expected.as_str());
            proptest::prop_assert_eq!(installation.effect_row.required_imports().specs().iter().cloned()
                .collect::<std::collections::BTreeSet<_>>(), imports.clone());
            let dispatcher = installation.dispatcher_effect_row();
            let mut expected_imports = imports;
            expected_imports.extend(["qualified Tidepool.Effects.Core".into(), "qualified Tidepool.Agent.Contract".into()]);
            proptest::prop_assert_eq!(dispatcher.required_imports().specs().iter().cloned()
                .collect::<std::collections::BTreeSet<_>>(), expected_imports);
            let authored = tidepool_runtime::session::SourceImports::from_specs([
                "qualified Tidepool.Agent.Watch.Internal as PublicWatch",
                "qualified Tidepool.Agent.Reply.Internal as PublicReply",
            ]);
            let combined = dispatcher.source_imports(&authored);
            proptest::prop_assert_eq!(dispatcher.source_imports(&combined), combined);
            // A plain authored expression never acquires guessed imports.
            proptest::prop_assert!(tidepool_runtime::session::HaskellTypeSource::from(expected)
                .required_imports().specs().is_empty());
        }
    }

    #[test]
    fn installer_effect_order_is_part_of_the_source_specialization() {
        let forward = installation_expression(
            "AgentSpec.agentSpec",
            &[
                crate::ActorEffectKey::Replies,
                crate::ActorEffectKey::Commands,
            ],
        );
        let reversed = installation_expression(
            "AgentSpec.agentSpec",
            &[
                crate::ActorEffectKey::Commands,
                crate::ActorEffectKey::Replies,
            ],
        );
        assert_ne!(forward, reversed);
        assert!(forward
            .effect_row
            .expression()
            .contains("Tidepool.Agent.Reply.Internal.Replies"));
    }
}
