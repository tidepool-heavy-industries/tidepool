//! Usage hints and registered examples from the workspace an actor runs in.
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tidepool_bridge_derive::ToHaskell;

#[derive(Clone, Debug, PartialEq, Eq, ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core", name = "LookupExample")]
pub(crate) struct LookupExample {
    pub(crate) locator: String,
    pub(crate) requirements: String,
    pub(crate) prerequisites: Vec<String>,
    pub(crate) source: String,
    pub(crate) source_identity: Option<String>,
    pub(crate) source_issue: Option<String>,
    pub(crate) setup: Vec<LookupSource>,
}

/// Immutable source captured by the usage inventory owner, never by navigation.
#[derive(Clone, Debug, PartialEq, Eq, ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core", name = "LookupSource")]
pub(crate) struct LookupSource {
    pub(crate) locator: String,
    pub(crate) identity: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) issue: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ExampleNamespace {
    Value,
    Type,
    Constructor,
    Field,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    examples: Vec<Registration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    module: String,
    name: String,
    namespace: ExampleNamespace,
    source: String,
    prerequisites: Vec<String>,
    requirements: String,
}

#[derive(Clone, Default)]
pub struct UsagePointerTable {
    workspace: Arc<PathBuf>,
    entries: Arc<BTreeMap<String, String>>,
    examples: Arc<BTreeMap<(String, String, ExampleNamespace), LookupExample>>,
}

impl UsagePointerTable {
    /// Read the active package, or the template package when no active copy exists.
    pub fn discover(workspace: &Path) -> std::io::Result<Self> {
        let active = workspace.join(".exomonad/workspace");
        let (root, prefix) = if active.is_dir() {
            (active, ".exomonad/workspace")
        } else {
            (workspace.join(".exomonad"), ".exomonad")
        };
        let mut sources = Vec::new();
        collect_sources(&root.join("checks"), &root, prefix, &mut sources)?;
        collect_sources(&root.join("skills"), &root, prefix, &mut sources)?;
        sources.sort_by(|left, right| left.0.cmp(&right.0));
        let mut index = BTreeMap::new();
        let mut captured = BTreeMap::new();
        let canonical_root = match root.canonicalize() {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    workspace: Arc::new(workspace.to_path_buf()),
                    ..Self::default()
                })
            }
            Err(error) => return Err(error),
        };
        for (locator, path) in sources {
            let canonical = path.canonicalize()?;
            if !canonical.starts_with(&canonical_root) {
                continue;
            }
            let source = std::fs::read_to_string(&path)?;
            captured.insert(path, Ok(source.clone()));
            for token in raw_tokens(&source) {
                let token = token.trim_matches('.');
                let Some((_, identifier)) = super::lookup_tool::qualifier_and_identifier(token)
                else {
                    continue;
                };
                index
                    .entry(identifier.to_owned())
                    .or_insert_with(|| locator.clone());
            }
        }
        let examples = match load_examples(&root, prefix, &mut captured) {
            Ok(examples) => examples,
            Err(error) => {
                eprintln!("lookup examples unavailable: {error}");
                BTreeMap::new()
            }
        };
        Ok(Self {
            workspace: Arc::new(workspace.to_path_buf()),
            entries: Arc::new(index),
            examples: Arc::new(examples),
        })
    }
}

fn load_examples(
    root: &Path,
    prefix: &str,
    captured: &mut BTreeMap<PathBuf, Result<String, String>>,
) -> Result<BTreeMap<(String, String, ExampleNamespace), LookupExample>, String> {
    let manifest_path = root.join("checks/usage-examples.json");
    let manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("{}: {error}", manifest_path.display())),
    };
    let parsed: Manifest = serde_json::from_str(&manifest)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let mut examples = BTreeMap::new();
    for registration in parsed.examples {
        let identity = format!(
            "{}.{} ({:?})",
            registration.module, registration.name, registration.namespace
        );
        let refusal =
            |reason: String| format!("{}: example {identity}: {reason}", manifest_path.display());
        if registration.module.is_empty()
            || registration.name.is_empty()
            || registration.requirements.trim().is_empty()
            || registration.requirements.len() > 1024
        {
            return Err(refusal(format!(
                "invalid identity or requirements ({} bytes; limit 1024)",
                registration.requirements.len()
            )));
        }
        // Invalid main paths reject the registration. A valid main path that
        // cannot be captured retains an explicit source failure. Prerequisite
        // capture failures, including invalid paths, remain per-setup issues.
        validate_example_path(&registration.source).map_err(&refusal)?;
        // This is a rendered metadata budget, not a prerequisite cardinality
        // limit. Check it before reading any example or setup source bodies.
        let locator_bytes = |path: &str| prefix.len().saturating_add(1).saturating_add(path.len());
        let metadata_bytes = registration
            .prerequisites
            .iter()
            .fold(
                locator_bytes(&registration.source).saturating_add(registration.requirements.len()),
                |bytes, path| bytes.saturating_add(locator_bytes(path)),
            )
            .saturating_add(
                registration
                    .prerequisites
                    .len()
                    .saturating_sub(1)
                    .saturating_mul(2),
            );
        // Reserve 256 bytes for renderer labels within its 2 KiB metadata allowance.
        if metadata_bytes > 1792 {
            return Err(refusal(format!(
                "metadata {metadata_bytes} bytes exceeds 1792-byte allowance"
            )));
        }
        let (source, source_identity, source_issue) =
            match capture_source(root, &canonical_root, &registration.source, captured) {
                Ok(source) => {
                    let identity = blake3::hash(source.as_bytes()).to_hex().to_string();
                    (source, Some(identity), None)
                }
                Err(issue) => (String::new(), None, Some(issue)),
            };
        let setup: Vec<_> = registration
            .prerequisites
            .iter()
            .map(|path| {
                let locator = format!("{prefix}/{path}");
                match capture_source(root, &canonical_root, path, captured) {
                    Ok(text) => LookupSource {
                        locator,
                        identity: Some(blake3::hash(text.as_bytes()).to_hex().to_string()),
                        text: Some(text),
                        issue: None,
                    },
                    Err(issue) => LookupSource {
                        locator,
                        identity: None,
                        text: None,
                        issue: Some(issue),
                    },
                }
            })
            .collect();
        let prerequisites = setup.iter().map(|source| source.locator.clone()).collect();
        let key = (
            registration.module,
            registration.name,
            registration.namespace,
        );
        let example = LookupExample {
            locator: format!("{prefix}/{}", registration.source),
            requirements: registration.requirements,
            prerequisites,
            source_identity,
            source_issue,
            source,
            setup,
        };
        if examples.insert(key, example).is_some() {
            return Err(refusal("duplicate example declaration identity".into()));
        }
    }
    Ok(examples)
}

fn validate_example_path(relative: &str) -> Result<(), String> {
    let path = Path::new(relative);
    if path.as_os_str().is_empty()
        || relative.len() > 256
        || path.extension().is_none_or(|extension| extension != "hs")
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!("invalid example path: {relative}"));
    }
    Ok(())
}

// One acquisition per file in this inventory construction, including failures.
// The temporary map is discarded after indexing and examples share its captures.
fn capture_source(
    root: &Path,
    canonical_root: &Path,
    relative: &str,
    captured: &mut BTreeMap<PathBuf, Result<String, String>>,
) -> Result<String, String> {
    validate_example_path(relative)?;
    captured
        .entry(root.join(relative))
        .or_insert_with(|| checked_source(root, canonical_root, relative))
        .clone()
}

fn checked_source(root: &Path, canonical_root: &Path, relative: &str) -> Result<String, String> {
    validate_example_path(relative)?;
    let path = Path::new(relative);
    let full = root.join(path);
    let canonical = full
        .canonicalize()
        .map_err(|error| format!("{}: {error}", full.display()))?;
    if !canonical.starts_with(canonical_root) {
        return Err(format!("example path escapes package: {relative}"));
    }
    std::fs::read_to_string(&full).map_err(|error| format!("{}: {error}", full.display()))
}

fn collect_sources(
    dir: &Path,
    root: &Path,
    prefix: &str,
    out: &mut Vec<(String, PathBuf)>,
) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_sources(&path, root, prefix, out)?;
        } else {
            #[allow(
                clippy::expect_used,
                reason = "path is reached only by recursing from root"
            )]
            let relative = path.strip_prefix(root).expect("path below root");
            let is_check = relative.starts_with("checks")
                && path.extension().is_some_and(|extension| extension == "hs");
            let is_skill = relative.starts_with("skills")
                && path.file_name().is_some_and(|name| name == "SKILL.md");
            if is_check || is_skill {
                out.push((format!("{prefix}/{}", relative.display()), path));
            }
        }
    }
    Ok(())
}

fn raw_tokens(source: &str) -> impl Iterator<Item = &str> {
    source.split(|character: char| {
        !(character.is_ascii_alphanumeric()
            || character == '_'
            || character == '\''
            || character == '.')
    })
}

pub(crate) fn pointer_for(table: &UsagePointerTable, name: &str) -> Option<String> {
    let locator = table.entries.get(name)?;
    present(locator, &table.workspace)
}

pub(crate) fn example_for(
    table: &UsagePointerTable,
    module: &str,
    name: &str,
    namespace: ExampleNamespace,
) -> Option<LookupExample> {
    table
        .examples
        .get(&(module.into(), name.into(), namespace))
        .cloned()
}

fn present(locator: &str, workspace: &Path) -> Option<String> {
    for prefix in [".exomonad/workspace/skills/", ".exomonad/skills/"] {
        if let Some(skill) = locator
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix("/SKILL.md"))
        {
            return Some(format!("skill {skill}"));
        }
    }
    workspace
        .join(locator)
        .is_file()
        .then(|| locator.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_current_workspace_skills() {
        let workspace = tempfile::tempdir().unwrap();
        let skill = workspace.path().join(".exomonad/workspace/skills/example");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "Use Command.call here.").unwrap();
        let table = UsagePointerTable::discover(workspace.path()).unwrap();
        assert_eq!(pointer_for(&table, "call"), Some("skill example".into()));
        assert_eq!(pointer_for(&table, "missing"), None);
    }
    #[test]
    fn check_hints_are_scoped_to_the_discovered_workspace() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let check = first.path().join(".exomonad/workspace/checks/route.hs");
        std::fs::create_dir_all(check.parent().unwrap()).unwrap();
        std::fs::write(&check, "Project.Routing.route").unwrap();
        let first_table = UsagePointerTable::discover(first.path()).unwrap();
        let second_table = UsagePointerTable::discover(second.path()).unwrap();
        assert_eq!(
            pointer_for(&first_table, "route"),
            Some(".exomonad/workspace/checks/route.hs".into())
        );
        assert_eq!(pointer_for(&second_table, "route"), None);
    }

    fn registered(workspace: &Path, package: &str) -> UsagePointerTable {
        let root = workspace.join(package);
        std::fs::create_dir_all(root.join("checks")).unwrap();
        std::fs::write(
            root.join("checks/example.hs"),
            "café <- Cmd.start command\n",
        )
        .unwrap();
        std::fs::write(
            root.join("checks/usage-examples.json"),
            r#"{
          "examples": [{
            "module": "Tidepool.Command", "name": "start", "namespace": "value",
            "source": "checks/example.hs", "prerequisites": [], "requirements": "Import Cmd"
          }]
        }"#,
        )
        .unwrap();
        UsagePointerTable::discover(workspace).unwrap()
    }

    #[test]
    fn registered_examples_use_exact_identity_in_both_package_layouts() {
        for package in [".exomonad/workspace", ".exomonad"] {
            let workspace = tempfile::tempdir().unwrap();
            let table = registered(workspace.path(), package);
            let example =
                example_for(&table, "Tidepool.Command", "start", ExampleNamespace::Value).unwrap();
            assert_eq!(example.locator, format!("{package}/checks/example.hs"));
            assert_eq!(example.source, "café <- Cmd.start command\n");
            assert!(
                example_for(&table, "Other.Command", "start", ExampleNamespace::Value).is_none()
            );
            assert!(
                example_for(&table, "Tidepool.Command", "start", ExampleNamespace::Type).is_none()
            );
            assert_eq!(pointer_for(&table, "start"), Some(example.locator));
        }
    }

    #[test]
    fn missing_and_invalid_registration_leave_usage_hints_intact() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join(".exomonad/workspace");
        std::fs::create_dir_all(root.join("checks")).unwrap();
        std::fs::write(root.join("checks/example.hs"), "Cmd.start command").unwrap();
        let absent = UsagePointerTable::discover(workspace.path()).unwrap();
        assert!(example_for(
            &absent,
            "Tidepool.Command",
            "start",
            ExampleNamespace::Value
        )
        .is_none());
        for source in ["../example.hs", "/tmp/example.hs"] {
            std::fs::write(
                root.join("checks/usage-examples.json"),
                format!(
                    r#"{{"examples":[{{
                "module":"Tidepool.Command","name":"start","namespace":"value",
                "source":"{source}","prerequisites":[],"requirements":"Import Cmd"}}]}}"#
                ),
            )
            .unwrap();
            let table = UsagePointerTable::discover(workspace.path()).unwrap();
            assert!(
                example_for(&table, "Tidepool.Command", "start", ExampleNamespace::Value).is_none()
            );
            assert_eq!(
                pointer_for(&table, "start"),
                Some(".exomonad/workspace/checks/example.hs".into())
            );
        }
    }

    #[test]
    fn more_than_four_prerequisites_retain_exact_sources_and_identity_links() {
        for package in [".exomonad/workspace", ".exomonad"] {
            let workspace = tempfile::tempdir().unwrap();
            let root = workspace.path().join(package);
            std::fs::create_dir_all(root.join("checks")).unwrap();
            std::fs::create_dir_all(root.join("Project")).unwrap();
            let main = "-- exact main\r\nmain = \"λ\"\n";
            std::fs::write(root.join("checks/example.hs"), main).unwrap();
            let setup: Vec<_> = (0..7)
                .map(|index| {
                    let path = format!("Project/Setup{index}.hs");
                    let text = format!("-- setup {index}\r\nvalue{index} = \"λ{index}\"\n");
                    std::fs::write(root.join(&path), &text).unwrap();
                    (path, text)
                })
                .collect();
            let rows: Vec<_> = ["operation", "other"].into_iter().map(|name| serde_json::json!({
                "module":"Owner", "name":name, "namespace":"value", "source":"checks/example.hs",
                "prerequisites":setup.iter().map(|(path, _)| path).collect::<Vec<_>>(), "requirements":"runtime"
            })).collect();
            std::fs::write(
                root.join("checks/usage-examples.json"),
                serde_json::json!({"examples":rows}).to_string(),
            )
            .unwrap();
            let table = UsagePointerTable::discover(workspace.path()).unwrap();
            for name in ["operation", "other"] {
                let example = example_for(&table, "Owner", name, ExampleNamespace::Value).unwrap();
                assert_eq!(example.source, main);
                assert_eq!(
                    example.source_identity,
                    Some(blake3::hash(main.as_bytes()).to_hex().to_string())
                );
                assert_eq!(example.source_issue, None);
                assert_eq!(example.setup.len(), setup.len());
                for (captured, (path, text)) in example.setup.iter().zip(&setup) {
                    assert_eq!(captured.locator, format!("{package}/{path}"));
                    assert_eq!(captured.text.as_ref(), Some(text));
                    assert_eq!(
                        captured.identity,
                        Some(blake3::hash(text.as_bytes()).to_hex().to_string())
                    );
                    assert_eq!(captured.issue, None);
                }
                assert_eq!(
                    example.prerequisites,
                    setup
                        .iter()
                        .map(|(path, _)| format!("{package}/{path}"))
                        .collect::<Vec<_>>()
                );
            }
            assert!(
                example_for(&table, "OtherOwner", "operation", ExampleNamespace::Value).is_none()
            );
            assert!(example_for(&table, "Owner", "operation", ExampleNamespace::Type).is_none());
        }
    }

    #[test]
    fn registration_refusals_identify_owner_and_preflight_capture_budgets() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path();
        std::fs::create_dir_all(root.join("checks")).unwrap();
        let valid = serde_json::json!({"module":"Owner", "name":"operation", "namespace":"value",
            "source":"checks/example.hs", "prerequisites":[], "requirements":"runtime"});
        let mut metadata = valid.clone();
        metadata["requirements"] = serde_json::json!("x".repeat(900));
        metadata["prerequisites"] = serde_json::json!((0..7)
            .map(|index| format!("Project/{}{index}.hs", "x".repeat(110)))
            .collect::<Vec<_>>());
        let mut path = valid.clone();
        path["source"] = serde_json::json!("../outside.hs");
        let mut requirements = valid.clone();
        requirements["requirements"] = serde_json::json!("x".repeat(1025));
        for (row, reason) in [
            (metadata, "1792"),
            (path, "../outside.hs"),
            (requirements, "1025"),
        ] {
            std::fs::write(
                root.join("checks/usage-examples.json"),
                serde_json::json!({"examples":[row]}).to_string(),
            )
            .unwrap();
            let mut captured = BTreeMap::new();
            let issue = load_examples(root, ".exomonad/workspace", &mut captured).unwrap_err();
            assert!(issue.contains("checks/usage-examples.json"), "{issue}");
            assert!(issue.contains("Owner.operation"), "{issue}");
            assert!(issue.contains(reason), "{issue}");
            assert!(
                captured.is_empty(),
                "refused registration read source bodies"
            );
        }
        std::fs::write(
            root.join("checks/usage-examples.json"),
            serde_json::json!({"examples":[valid.clone(),valid]}).to_string(),
        )
        .unwrap();
        let issue = load_examples(root, ".exomonad/workspace", &mut BTreeMap::new()).unwrap_err();
        assert!(
            issue.contains("Owner.operation") && issue.contains("duplicate"),
            "{issue}"
        );
    }

    #[test]
    fn captured_example_and_setup_survive_source_mutation_and_missing_files() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join(".exomonad/workspace");
        std::fs::create_dir_all(root.join("checks")).unwrap();
        std::fs::write(root.join("checks/main.hs"), "main = pure ()\r\n").unwrap();
        std::fs::write(root.join("checks/setup.hs"), "setup = \"α\"\n").unwrap();
        let manifest = serde_json::json!({"examples":[{
            "module":"Owner", "name":"operation", "namespace":"value",
            "source":"checks/main.hs", "prerequisites":["checks/setup.hs", "checks/missing.hs"],
            "requirements":"network credentials at runtime"
        }]});
        std::fs::write(
            root.join("checks/usage-examples.json"),
            manifest.to_string(),
        )
        .unwrap();
        let table = UsagePointerTable::discover(workspace.path()).unwrap();
        let captured = example_for(&table, "Owner", "operation", ExampleNamespace::Value).unwrap();
        assert_eq!(captured.source, "main = pure ()\r\n");
        assert_eq!(
            captured.source_identity.as_deref(),
            Some(blake3::hash(captured.source.as_bytes()).to_hex().as_str())
        );
        assert_eq!(captured.setup[0].text.as_deref(), Some("setup = \"α\"\n"));
        assert!(captured.setup[1].text.is_none());
        assert!(captured.setup[1].issue.is_some());
        assert_eq!(captured.requirements, "network credentials at runtime");
        std::fs::write(root.join("checks/main.hs"), "changed").unwrap();
        std::fs::remove_file(root.join("checks/setup.hs")).unwrap();
        std::fs::write(root.join("checks/missing.hs"), "now exists").unwrap();
        assert_eq!(
            example_for(&table, "Owner", "operation", ExampleNamespace::Value),
            Some(captured)
        );
    }

    #[test]
    fn missing_main_example_is_explicit_capture_failure() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join(".exomonad/workspace");
        std::fs::create_dir_all(root.join("checks")).unwrap();
        std::fs::write(root.join("checks/usage-examples.json"),r#"{"examples":[{"module":"Owner","name":"operation","namespace":"value","source":"checks/missing.hs","prerequisites":[],"requirements":"runtime"}]}"#).unwrap();
        let table = UsagePointerTable::discover(workspace.path()).unwrap();
        let example = example_for(&table, "Owner", "operation", ExampleNamespace::Value).unwrap();
        assert!(example.source.is_empty());
        assert!(example.source_identity.is_none());
        assert!(example.source_issue.is_some());
    }

    #[test]
    fn oversized_multibyte_metadata_cannot_register_a_truncated_pointer() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join(".exomonad/workspace");
        std::fs::create_dir_all(root.join("checks")).unwrap();
        std::fs::write(root.join("checks/example.hs"), "Cmd.start command").unwrap();
        let long_path = format!("checks/{}.hs", "x".repeat(240));
        std::fs::write(root.join(&long_path), "prerequisite").unwrap();
        let prerequisites = vec![long_path; 4];
        let registration = serde_json::json!({"examples":[{
            "module":"Tidepool.Command", "name":"start", "namespace":"value",
            "source":"checks/example.hs", "prerequisites": prerequisites,
            "requirements":"é".repeat(500)
        }]});
        std::fs::write(
            root.join("checks/usage-examples.json"),
            registration.to_string(),
        )
        .unwrap();
        let table = UsagePointerTable::discover(workspace.path()).unwrap();
        assert!(
            example_for(&table, "Tidepool.Command", "start", ExampleNamespace::Value).is_none()
        );
        assert_eq!(
            pointer_for(&table, "start"),
            Some(".exomonad/workspace/checks/example.hs".into())
        );
    }
}
