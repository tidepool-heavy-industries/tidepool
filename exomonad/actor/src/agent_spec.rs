//! Where one actor's spec comes from.
//!
//! A spec is one Haskell module in the run's current tooling graph, exporting
//! one value. It is found by convention before the configured workspace keys.
//!
//! Discovery being implicit carries one obligation, which is the whole reason
//! this module answers a value rather than a string: the rule that matched, the
//! roots that were searched, and the file the entry was read from are reported
//! in status and in every reload receipt. A model must never have to guess
//! which spec is live, and a spec that was not found because it sits outside a
//! declared source root must be able to see the list of roots that were looked
//! in.

use std::path::{Path, PathBuf};

/// The two names the whole convention consists of: the module, and the value
/// it exports.
pub(crate) const SPEC_MODULE: &str = "AgentSpec";
pub(crate) const SPEC_VALUE: &str = "agentSpec";

/// Which of the four rules answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecRule {
    /// `AgentSpec.agentSpec`, found in the active include roots.
    RunModule,
    /// The workspace's `[haskell] spec` key, for a workspace that wants
    /// another name.
    WorkspaceSpec,
    /// The existing `[haskell] tools` entry point.
    WorkspaceTools,
    /// Nothing named a spec, and nothing is installed.
    BuiltinDefault,
}

impl SpecRule {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::RunModule => "run module",
            Self::WorkspaceSpec => "workspace spec key",
            Self::WorkspaceTools => "workspace tools key",
            Self::BuiltinDefault => "built-in default",
        }
    }
}

/// What one compile installs, and which rule chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSpec {
    pub rule: SpecRule,
    /// The qualified Haskell value the install fragment applies. `None` under
    /// [`SpecRule::BuiltinDefault`]: nothing is named, so nothing is
    /// installed and the actor behaves exactly as it does with no spec.
    pub entry: Option<String>,
    /// The file the entry was read from, when discovery found one on disk.
    /// A configured entry point names a module, not a path, so it has none.
    pub file: Option<PathBuf>,
    /// The source roots that were searched for the spec module, in the order
    /// GHC would have taken them.
    pub searched: Vec<PathBuf>,
}

impl ResolvedSpec {
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

/// Resolve one actor's spec, stopping at the first rule that answers.
///
/// `layer` is the exact include graph this actor compiles against, private
/// helper roots first and shared run roots after them. Helper roots may only
/// contain `SessionHelpers`, so a discovered `AgentSpec` comes from the run.
pub fn resolve(layer: &[PathBuf], spec: Option<&str>, tools: Option<&str>) -> ResolvedSpec {
    let searched: Vec<PathBuf> = layer.to_vec();
    if let Some(file) = layer.iter().find_map(|root| {
        let candidate = root.join(format!("{SPEC_MODULE}.hs"));
        candidate.is_file().then_some(candidate)
    }) {
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
    if let Some(entry) = tools {
        return ResolvedSpec {
            rule: SpecRule::WorkspaceTools,
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

/// Effect names named in `Member <Effect> ...` constraints in the type
/// signature `source` gives `value`, in the order they first appear.
///
/// This is a textual read of exactly the context GHC would need to accept a
/// call into that effect, spelled the same way
/// [`crate::ActorEffectKey::haskell_name`] spells it — not a type check.
/// `exomonad check --workspace` uses it to catch what a role's effect row is
/// missing before a child is ever admitted, rather than after; it does not
/// replace GHC, and a spec that hides its requirement behind polymorphism
/// GHC infers rather than an explicit `Member` constraint is invisible to it.
#[must_use]
pub fn required_effects_from_signature(source: &str, value: &str) -> Vec<String> {
    let needle = format!("{value} ::");
    let Some(start) = source.find(&needle) else {
        return Vec::new();
    };
    let rest = &source[start + needle.len()..];
    // A context always ends at `=>`; a signature with none has no
    // constraints to read, so there is nothing more to find.
    let Some(context_end) = rest.find("=>") else {
        return Vec::new();
    };
    let context = &rest[..context_end];
    let mut names = Vec::new();
    let mut cursor = context;
    while let Some(index) = cursor.find("Member") {
        let after = cursor[index + "Member".len()..].trim_start();
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '\'')
            .collect();
        cursor = &after[name.len()..];
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_effects_reads_the_member_constraints_of_the_named_value() {
        let source = r#"
module AgentSpec (agentSpec) where

agentSpec ::
  ( Member Commands effects, Member Lookup effects, Member Journal effects
  ) =>
  AgentSpec Tools.WorkspaceTools effects
agentSpec = defaultSpec { specTools = Tools.tools }
"#;
        assert_eq!(
            required_effects_from_signature(source, "agentSpec"),
            vec![
                "Commands".to_owned(),
                "Lookup".to_owned(),
                "Journal".to_owned()
            ]
        );
    }

    #[test]
    fn a_signature_with_no_context_requires_nothing() {
        let source =
            "agentSpec :: AgentSpec Tools.WorkspaceTools effects\nagentSpec = defaultSpec\n";
        assert!(required_effects_from_signature(source, "agentSpec").is_empty());
    }

    #[test]
    fn an_absent_value_requires_nothing() {
        assert!(required_effects_from_signature("module X where\n", "agentSpec").is_empty());
    }

    /// Rule one wins over both configured keys, and names the file it was read
    /// from, so a model never has to guess which spec is live.
    #[test]
    fn a_spec_module_in_the_run_graph_answers_first() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("AgentSpec.hs"), "module AgentSpec where\n").unwrap();
        let resolved = resolve(
            &[root.path().to_path_buf()],
            Some("Project.Spec.agentSpec"),
            Some("Project.Tools.tools"),
        );
        assert_eq!(resolved.rule, SpecRule::RunModule);
        assert_eq!(resolved.entry.as_deref(), Some("AgentSpec.agentSpec"));
        assert_eq!(resolved.file, Some(root.path().join("AgentSpec.hs")));
        assert_eq!(resolved.checked_module().as_deref(), Some("AgentSpec"));
    }

    /// A run graph with no spec file uses the workspace's
    /// keys decide, in their existing order.
    #[test]
    fn a_run_without_a_spec_file_falls_through_in_order() {
        let root = tempfile::tempdir().unwrap();
        let layer = [root.path().to_path_buf()];
        let with_spec = resolve(
            &layer,
            Some("Project.Spec.agentSpec"),
            Some("P.Tools.tools"),
        );
        assert_eq!(with_spec.rule, SpecRule::WorkspaceSpec);
        assert_eq!(with_spec.entry.as_deref(), Some("Project.Spec.agentSpec"));

        let tools_only = resolve(&layer, None, Some("P.Tools.tools"));
        assert_eq!(tools_only.rule, SpecRule::WorkspaceTools);
        assert_eq!(tools_only.entry.as_deref(), Some("P.Tools.tools"));

        let nothing = resolve(&layer, None, None);
        assert_eq!(nothing.rule, SpecRule::BuiltinDefault);
        assert_eq!(nothing.entry, None);
        assert_eq!(nothing.checked_module(), None);
    }

    /// With no include roots, configured keys still determine the rule.
    #[test]
    fn an_actor_without_a_layer_reports_the_configured_rule() {
        let resolved = resolve(&[], None, Some("Tidepool.Command.Tools.tools"));
        assert_eq!(resolved.rule, SpecRule::WorkspaceTools);
        assert!(resolved.searched.is_empty());
        assert!(resolved.describe().contains("workspace tools key"));
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
            &[root.path().join("active/0")],
            None,
            Some("Project.Tools.tools"),
        );
        assert_eq!(resolved.source_revision().as_deref(), Some("revision-one"));
    }
}
