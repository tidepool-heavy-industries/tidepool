//! Where one actor's spec comes from.
//!
//! A spec is one Haskell module in the actor's own checkout, exporting one
//! value. It is found by convention rather than by configuration, because the
//! configuration that exists cannot express it: `[haskell] tools` is one
//! workspace-global key, resolved once at composition-root construction and
//! threaded identically into every actor, and a spec per checkout is exactly
//! what that is not.
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
    /// `AgentSpec.agentSpec`, found in a source root of the actor's own
    /// checkout.
    CheckoutModule,
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
            Self::CheckoutModule => "checkout module",
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
    /// One line naming the rule and the file, for status and for a reload
    /// receipt.
    pub(crate) fn describe(&self) -> String {
        let entry = self.entry.as_deref().unwrap_or("(none)");
        match &self.file {
            // The file is read from a published revision deep in the run's
            // cache. A reader edits the copy in their own source roots, so the
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
/// `layer` is the actor's own source layer — the include roots it alone
/// compiles against, ahead of everything shared. An actor with no checkout of
/// its own carries an empty layer, so rule one cannot answer for it and the
/// workspace's keys decide, which is exactly today's behaviour.
pub fn resolve(layer: &[PathBuf], spec: Option<&str>, tools: Option<&str>) -> ResolvedSpec {
    let searched: Vec<PathBuf> = layer.to_vec();
    if let Some(file) = layer.iter().find_map(|root| {
        let candidate = root.join(format!("{SPEC_MODULE}.hs"));
        candidate.is_file().then_some(candidate)
    }) {
        return ResolvedSpec {
            rule: SpecRule::CheckoutModule,
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

/// The revision an actor's own source layer currently resolves to.
///
/// Carried from install, not tracked: a layer's include roots are
/// `<layer>/active/<index>`, `active` is a symlink to `revisions/<identity>`,
/// and the identity is the resolved directory's own name. An actor with no
/// layer of its own has none to name, and says so by answering `None` rather
/// than borrowing the run's.
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
    fn a_spec_module_in_the_checkout_answers_first() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("AgentSpec.hs"), "module AgentSpec where\n").unwrap();
        let resolved = resolve(
            &[root.path().to_path_buf()],
            Some("Project.Spec.agentSpec"),
            Some("Project.Tools.tools"),
        );
        assert_eq!(resolved.rule, SpecRule::CheckoutModule);
        assert_eq!(resolved.entry.as_deref(), Some("AgentSpec.agentSpec"));
        assert_eq!(resolved.file, Some(root.path().join("AgentSpec.hs")));
        assert_eq!(resolved.checked_module().as_deref(), Some("AgentSpec"));
    }

    /// A checkout with no spec file behaves exactly as today: the workspace's
    /// keys decide, in their existing order.
    #[test]
    fn a_checkout_without_a_spec_file_falls_through_in_order() {
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

    /// An actor with no layer of its own searches nothing and names nothing,
    /// which is the ordinary case for every actor without a checkout.
    #[test]
    fn an_actor_without_a_layer_reports_the_configured_rule() {
        let resolved = resolve(&[], None, Some("Tidepool.Command.Tools.tools"));
        assert_eq!(resolved.rule, SpecRule::WorkspaceTools);
        assert!(resolved.searched.is_empty());
        assert!(resolved.describe().contains("workspace tools key"));
        assert_eq!(layer_revision(&[]), None);
    }
}
