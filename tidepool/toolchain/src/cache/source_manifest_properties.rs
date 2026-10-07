use super::*;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, FileFailurePersistence, TestCaseError, TestRunner};
use std::collections::BTreeMap;

const DIRS: [&str; 2] = ["left", "right"];
const NAMES: [&str; 6] = [
    "A.hs",
    "A.hs-boot",
    "B.lhs",
    "B.lhs-boot",
    "Prelude_cbor/Hidden.hs",
    "ignored.txt",
];

#[derive(Clone, Debug)]
enum Op {
    Write(u8, u8, u8),
    Rename(u8, u8, u8),
    Remove(u8, u8),
    Alias(u8, bool),
}

fn operations() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            (0u8..2, 0u8..6, 0u8..4).prop_map(|(d, n, value)| Op::Write(d, n, value)),
            (0u8..2, 0u8..6, 0u8..6).prop_map(|(d, from, to)| Op::Rename(d, from, to)),
            (0u8..2, 0u8..6).prop_map(|(d, n)| Op::Remove(d, n)),
            (0u8..2, any::<bool>()).prop_map(|(alias, enabled)| Op::Alias(alias, enabled)),
        ],
        1..36,
    )
}

fn property_config() -> Config {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 128;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

fn rel(dir: u8, name: u8) -> PathBuf {
    PathBuf::from(DIRS[dir as usize]).join(NAMES[name as usize])
}

fn bytes(value: u8) -> Vec<u8> {
    // Equal-length contents make digest changes observable without relying on size.
    format!("source-{value:02}").into_bytes()
}

fn expected(files: &BTreeMap<PathBuf, Vec<u8>>, shipped: bool) -> Vec<(PathBuf, String)> {
    files
        .iter()
        .filter(|(path, _)| {
            let dependency = [".hs", ".hs-boot", ".lhs", ".lhs-boot"]
                .iter()
                .any(|suffix| {
                    path.file_name()
                        .unwrap()
                        .as_encoded_bytes()
                        .ends_with(suffix.as_bytes())
                });
            dependency
                && (!shipped
                    || (path.extension().is_some_and(|extension| extension == "hs")
                        && !path
                            .components()
                            .any(|component| component.as_os_str() == "Prelude_cbor")))
        })
        .map(|(path, contents)| (path.clone(), blake3::hash(contents).to_hex().to_string()))
        .collect()
}

fn materialize(root: &Path, files: &BTreeMap<PathBuf, Vec<u8>>, aliases: [bool; 2]) {
    fs::create_dir_all(root).unwrap();
    for directory in DIRS {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    for (path, contents) in files {
        let destination = root.join(path);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(destination, contents).unwrap();
    }
    #[cfg(unix)]
    for (index, enabled) in aliases.into_iter().enumerate() {
        if enabled {
            std::os::unix::fs::symlink(root.join("left"), root.join(format!("alias{index}")))
                .unwrap();
        }
    }
}

fn with_alias(
    files: &BTreeMap<PathBuf, Vec<u8>>,
    aliases: [bool; 2],
) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut visible = files.clone();
    for (index, alias) in aliases.into_iter().enumerate() {
        if alias {
            for (path, contents) in files {
                if path.starts_with("left") {
                    visible.insert(
                        Path::new(&format!("alias{index}"))
                            .join(path.strip_prefix("left").unwrap()),
                        contents.clone(),
                    );
                }
            }
        }
    }
    visible
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Coverage {
    writes: usize,
    replacements: usize,
    renames: usize,
    removes: usize,
    recreations: usize,
    alias_adds: usize,
    alias_removes: usize,
    missing_renames: usize,
    missing_removals: usize,
    unchanged_aliases: usize,
}

fn replay(ops: &[Op]) -> Result<Coverage, TestCaseError> {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("tree");
    fs::create_dir(&root).unwrap();
    for directory in DIRS {
        fs::create_dir(root.join(directory)).unwrap();
    }
    let mut files = BTreeMap::<PathBuf, Vec<u8>>::new();
    let mut aliases = [false; 2];
    let mut removed_paths = std::collections::HashSet::new();
    let mut covered = Coverage::default();
    for op in ops {
        match *op {
            Op::Write(dir, name, value) => {
                let path = rel(dir, name);
                let contents = bytes(value);
                if let Some(old) = files.get(&path) {
                    if old != &contents {
                        covered.replacements += 1;
                    }
                } else if removed_paths.remove(&path) {
                    covered.recreations += 1;
                }
                let destination = root.join(&path);
                fs::create_dir_all(destination.parent().unwrap()).unwrap();
                fs::write(destination, &contents).unwrap();
                files.insert(path, contents);
                covered.writes += 1;
            }
            Op::Rename(dir, from, to) => {
                let from = rel(dir, from);
                let to = rel(dir, to);
                if from != to {
                    if let Some(contents) = files.get(&from).cloned() {
                        let destination = root.join(&to);
                        fs::create_dir_all(destination.parent().unwrap()).unwrap();
                        fs::rename(root.join(&from), destination).unwrap();
                        files.remove(&to);
                        files.remove(&from);
                        files.insert(to, contents);
                        covered.renames += 1;
                    } else {
                        covered.missing_renames += 1;
                    }
                }
            }
            Op::Remove(dir, name) => {
                let path = rel(dir, name);
                if files.remove(&path).is_some() {
                    fs::remove_file(root.join(&path)).unwrap();
                    removed_paths.insert(path);
                    covered.removes += 1;
                } else {
                    covered.missing_removals += 1;
                }
            }
            Op::Alias(which, enabled) => {
                let index = which as usize;
                match (aliases[index], enabled) {
                    (false, true) => {
                        #[cfg(unix)]
                        std::os::unix::fs::symlink(
                            root.join("left"),
                            root.join(format!("alias{index}")),
                        )
                        .unwrap();
                        aliases[index] = true;
                        covered.alias_adds += 1;
                    }
                    (true, false) => {
                        fs::remove_file(root.join(format!("alias{index}"))).unwrap();
                        aliases[index] = false;
                        covered.alias_removes += 1;
                    }
                    _ => covered.unchanged_aliases += 1,
                }
            }
        }

        let visible = with_alias(&files, aliases);
        let expected_dependencies = expected(&visible, false);
        prop_assert_eq!(
            source_root_manifest(&root).unwrap(),
            expected_dependencies.clone()
        );
        prop_assert_eq!(
            shipped_haskell_source_manifest(&root)
                .unwrap()
                .files
                .into_iter()
                .map(|(path, digest)| (path, digest.to_hex().to_string()))
                .collect::<Vec<_>>(),
            expected(&visible, true)
        );

        let manifest = SourceRootManifest::from_file_digests(expected_dependencies.clone())
            .expect("oracle paths are valid source paths");
        let identity = source_roots_identity(b"history", &[root.clone()]).unwrap();
        prop_assert_eq!(
            identity.clone(),
            source_manifests_identity(b"history", &[manifest.clone()])
        );

        let relocated = tempfile::tempdir().unwrap();
        let relocated_root = relocated.path().join("tree");
        fs::create_dir(&relocated_root).unwrap();
        materialize(&relocated_root, &files, aliases);
        prop_assert_eq!(
            identity,
            source_roots_identity(b"history", &[relocated_root]).unwrap()
        );

        let other = tempfile::tempdir().unwrap();
        let second_root = other.path().join("tree");
        fs::create_dir(&second_root).unwrap();
        fs::write(second_root.join("Different.hs"), b"other").unwrap();
        let second_manifest =
            SourceRootManifest::from_file_digests(source_root_manifest(&second_root).unwrap())
                .unwrap();
        let forward =
            source_manifests_identity(b"order", &[manifest.clone(), second_manifest.clone()]);
        let reverse = source_manifests_identity(b"order", &[second_manifest, manifest.clone()]);
        if !manifest.files.is_empty() {
            prop_assert_ne!(forward, reverse);
        }
    }
    Ok(covered)
}

#[test]
fn source_manifest_history_supports_replacement_rename_recreation_and_aliases() {
    let ops = [
        Op::Remove(1, 3),
        Op::Rename(1, 3, 4),
        Op::Write(0, 0, 0),
        Op::Write(0, 0, 1),
        Op::Alias(0, true),
        Op::Alias(1, true),
        Op::Rename(0, 0, 1),
        Op::Remove(0, 1),
        Op::Write(0, 1, 2),
        Op::Alias(0, false),
    ];
    let covered = replay(&ops).unwrap();
    assert_eq!(covered.writes, 3);
    assert_eq!(covered.replacements, 1);
    assert_eq!(covered.renames, 1);
    assert_eq!(covered.removes, 1);
    assert_eq!(covered.recreations, 1);
    assert_eq!(covered.alias_adds, 2);
    assert_eq!(covered.alias_removes, 1);
    assert_eq!(covered.missing_renames, 1);
    assert_eq!(covered.missing_removals, 1);

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("Module.hs");
    fs::write(&source, b"source-00").unwrap();
    let before = source_roots_identity(b"support", &[root.path().to_path_buf()]).unwrap();
    fs::write(&source, b"source-01").unwrap();
    let replaced = source_roots_identity(b"support", &[root.path().to_path_buf()]).unwrap();
    assert_ne!(
        before, replaced,
        "same-size byte replacement changes identity"
    );

    let renamed = root.path().join("Renamed.hs");
    fs::rename(&source, &renamed).unwrap();
    let after_rename = source_roots_identity(b"support", &[root.path().to_path_buf()]).unwrap();
    assert_ne!(replaced, after_rename, "visible path changes identity");
    assert_eq!(
        source_root_manifest(root.path()).unwrap(),
        vec![(
            PathBuf::from("Renamed.hs"),
            blake3::hash(b"source-01").to_hex().to_string()
        )]
    );
}

#[test]
fn source_manifest_generated_histories_reach_mutation_transitions() {
    let strategy = operations();
    let mut runner = TestRunner::deterministic();
    let mut total = Coverage::default();
    for _ in 0..64 {
        let tree = strategy.new_tree(&mut runner).unwrap();
        let history_coverage = replay(&tree.current()).unwrap();
        total.writes += history_coverage.writes;
        total.replacements += history_coverage.replacements;
        total.renames += history_coverage.renames;
        total.removes += history_coverage.removes;
        total.recreations += history_coverage.recreations;
        total.alias_adds += history_coverage.alias_adds;
        total.alias_removes += history_coverage.alias_removes;
        total.missing_renames += history_coverage.missing_renames;
        total.missing_removals += history_coverage.missing_removals;
        total.unchanged_aliases += history_coverage.unchanged_aliases;
    }

    eprintln!("source manifest history coverage over 64 deterministic histories: {total:?}");
    assert!(total.writes > 0);
    assert!(total.replacements > 0);
    assert!(total.renames > 0);
    assert!(total.removes > 0);
    assert!(total.recreations > 0);
    assert!(total.alias_adds > 0);
    assert!(total.alias_removes > 0);
    assert!(total.missing_renames > 0);
    assert!(total.missing_removals > 0);
}

#[cfg(unix)]
#[test]
fn source_manifest_rejects_ancestor_cycles_and_metadata_failures() {
    let root = tempfile::tempdir().unwrap();
    let cycle_root = root.path().join("cycle");
    fs::create_dir_all(cycle_root.join("nested")).unwrap();
    std::os::unix::fs::symlink(&cycle_root, cycle_root.join("nested/back")).unwrap();
    let error = source_root_manifest(&cycle_root).unwrap_err();
    assert_eq!(error.source.kind(), std::io::ErrorKind::InvalidData);

    let dangling_root = root.path().join("dangling");
    fs::create_dir(&dangling_root).unwrap();
    std::os::unix::fs::symlink(
        dangling_root.join("absent"),
        dangling_root.join("ignored.txt"),
    )
    .unwrap();
    let error = source_root_manifest(&dangling_root).unwrap_err();
    assert_eq!(error.path, dangling_root.join("ignored.txt"));
    assert_eq!(error.source.kind(), std::io::ErrorKind::NotFound);

    let shipped_root = root.path().join("shipped");
    fs::create_dir(&shipped_root).unwrap();
    std::os::unix::fs::symlink(
        shipped_root.join("absent"),
        shipped_root.join("Prelude_cbor"),
    )
    .unwrap();
    assert!(
        shipped_haskell_source_manifest(&shipped_root)
            .unwrap()
            .files
            .is_empty()
    );
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn source_manifest_histories_match_recomputed_files(ops in operations()) {
        let _coverage = replay(&ops)?;
    }
}
