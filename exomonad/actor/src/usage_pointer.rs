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
        for (locator, path) in sources {
            let source = std::fs::read_to_string(path)?;
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
        let examples = match load_examples(&root, prefix) {
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
        if registration.module.is_empty()
            || registration.name.is_empty()
            || registration.requirements.trim().is_empty()
            || registration.requirements.len() > 1024
            || registration.prerequisites.len() > 4
        {
            return Err("example identity or requirements invalid".into());
        }
        let source = checked_source(root, &canonical_root, &registration.source)?;
        let prerequisites = registration
            .prerequisites
            .iter()
            .map(|path| {
                checked_source(root, &canonical_root, path).map(|_| format!("{prefix}/{path}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let key = (
            registration.module,
            registration.name,
            registration.namespace,
        );
        let example = LookupExample {
            locator: format!("{prefix}/{}", registration.source),
            requirements: registration.requirements,
            prerequisites,
            source,
        };
        // Reserve 256 bytes for the Haskell renderer's labels and omission
        // sentence; this owner only limits data, never duplicates presentation.
        let metadata_bytes = example.locator.len()
            + example.requirements.len()
            + example.prerequisites.iter().map(String::len).sum::<usize>()
            + example.prerequisites.len().saturating_sub(1) * 2;
        if metadata_bytes > 1792 {
            return Err("example metadata exceeds 2 KiB allowance".into());
        }
        if examples.insert(key, example).is_some() {
            return Err("duplicate example declaration identity".into());
        }
    }
    Ok(examples)
}

fn checked_source(root: &Path, canonical_root: &Path, relative: &str) -> Result<String, String> {
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
        for source in ["../example.hs", "checks/missing.hs", "/tmp/example.hs"] {
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
