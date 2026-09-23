//! Usage hints discovered from the workspace an actor actually runs in.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Bare identifier to workspace locator, shared by actors in one forest.
#[derive(Clone, Default)]
pub struct UsagePointerTable {
    workspace: Arc<PathBuf>,
    entries: Arc<BTreeMap<String, String>>,
}

impl UsagePointerTable {
    /// Read check scripts and skill instructions from this project's workspace.
    pub fn discover(workspace: &Path) -> std::io::Result<Self> {
        let root = workspace.join(".exomonad/workspace");
        let mut sources = Vec::new();
        collect_sources(&root.join("checks"), &root, &mut sources)?;
        collect_sources(&root.join("skills"), &root, &mut sources)?;
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
        Ok(Self {
            workspace: Arc::new(workspace.to_path_buf()),
            entries: Arc::new(index),
        })
    }
}

fn collect_sources(
    dir: &Path,
    root: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_sources(&path, root, out)?;
        } else {
            let relative = path.strip_prefix(root).expect("path below root");
            let is_check = relative.starts_with("checks")
                && path.extension().is_some_and(|extension| extension == "hs");
            let is_skill = relative.starts_with("skills")
                && path.file_name().is_some_and(|name| name == "SKILL.md");
            if is_check || is_skill {
                out.push((format!(".exomonad/workspace/{}", relative.display()), path));
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

fn present(locator: &str, workspace: &Path) -> Option<String> {
    if let Some(skill) = locator
        .strip_prefix(".exomonad/workspace/skills/")
        .and_then(|rest| rest.strip_suffix("/SKILL.md"))
    {
        return Some(format!("skill {skill}"));
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
}
