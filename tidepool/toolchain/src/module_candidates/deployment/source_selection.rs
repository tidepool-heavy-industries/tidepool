//! Original source paths selected together by native catalog production.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    absolute, io, reject_source_aliases, require_immutable_roots, ModulePackageError, RootPolicy,
};

/// The ordered native runtime source roles. Order determines import shadowing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSourceRole {
    StableEffects,
    Stdlib,
    Actors,
    Jev,
}

impl NativeSourceRole {
    pub const ORDERED: [Self; 4] = [Self::StableEffects, Self::Stdlib, Self::Actors, Self::Jev];

    pub fn relative_root(self) -> &'static str {
        match self {
            Self::StableEffects => "effects",
            Self::Stdlib => "lib",
            Self::Actors => "actors",
            Self::Jev => "jev/core",
        }
    }
}

/// One catalog witness binds its original relative path to SHA-256 source bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeCatalogSourceFile {
    pub path: PathBuf,
    pub sha256: String,
}

impl<'de> Deserialize<'de> for NativeCatalogSourceFile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SourceFileVisitor;
        impl<'de> serde::de::Visitor<'de> for SourceFileVisitor {
            type Value = NativeCatalogSourceFile;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a catalog source object with path and sha256 fields")
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut path = None;
                let mut sha256 = None;
                while let Some(field) = map.next_key::<String>()? {
                    match field.as_str() {
                        "path" if path.is_none() => path = Some(map.next_value()?),
                        "sha256" if sha256.is_none() => sha256 = Some(map.next_value()?),
                        "path" => return Err(serde::de::Error::duplicate_field("path")),
                        "sha256" => return Err(serde::de::Error::duplicate_field("sha256")),
                        _ => {
                            return Err(serde::de::Error::unknown_field(
                                &field,
                                &["path", "sha256"],
                            ))
                        }
                    }
                }
                Ok(NativeCatalogSourceFile {
                    path: path.ok_or_else(|| serde::de::Error::missing_field("path"))?,
                    sha256: sha256.ok_or_else(|| serde::de::Error::missing_field("sha256"))?,
                })
            }
        }
        deserializer.deserialize_map(SourceFileVisitor)
    }
}

/// Serialized source provenance; admission remains the catalog owner's job.
/// This value does not establish Nix registration or retention.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCatalogSourceSelection {
    pub snapshot_root: PathBuf,
    pub roles: [NativeSourceRole; 4],
    pub source_files: Vec<NativeCatalogSourceFile>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};

    #[test]
    fn rust_source_selection_serialization_matches_python_qualification() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        for role in NativeSourceRole::ORDERED {
            fs::create_dir_all(root.join(role.relative_root())).unwrap();
        }
        for (relative, bytes) in [
            ("TidepoolCatalog.hs", "module TidepoolCatalog where\n"),
            ("effects/Effect.hs", "module Effect where\n"),
            ("lib/Library.hs-boot", "module Library where\n"),
            ("actors/Actor.lhs", "> module Actor where\n"),
            ("jev/core/Jev.lhs-boot", "> module Jev where\n"),
            ("lib/ignored.c", "/* not a Haskell source */\n"),
        ] {
            fs::write(root.join(relative), bytes).unwrap();
        }
        let selection = NativeCatalogSourceSelection::capture_under(&root, RootPolicy::Fixture)
            .expect("genuine catalog SHA-256 capture");
        selection.validate_under(RootPolicy::Fixture).unwrap();
        let mut legacy = serde_json::to_value(&selection).unwrap();
        legacy["source_files"] = serde_json::json!([["TidepoolCatalog.hs", "0".repeat(64)]]);
        assert!(serde_json::from_value::<NativeCatalogSourceSelection>(legacy).is_err());
        let generic = crate::cache::source_root_manifest(&root).unwrap();
        assert_eq!(generic.len(), selection.source_files.len());
        for ((path, blake3), file) in generic.iter().zip(&selection.source_files) {
            assert_eq!(path, &file.path);
            assert_eq!(
                blake3,
                &blake3::hash(&fs::read(root.join(path)).unwrap())
                    .to_hex()
                    .to_string()
            );
            assert_ne!(blake3, &file.sha256);
        }
        let catalog = root.join("catalog.json");
        fs::write(
            &catalog,
            serde_json::to_vec(&serde_json::json!({
                "schema": 4,
                "source_selection": selection,
            }))
            .unwrap(),
        )
        .unwrap();
        let python = std::env::var_os("TIDEPOOL_CATALOG_TEST_PYTHON")
            .expect("declared pinned catalog interop Python");
        let qualification = std::env::var_os("TIDEPOOL_CATALOG_QUALIFICATION_SCRIPT")
            .expect("declared production qualification script");
        let output = Command::new(python)
            .args(["-I", "-B", "-c"])
            .arg(
                r#"
import importlib.machinery, importlib.util, json, sys
from pathlib import Path
loader = importlib.machinery.SourceFileLoader("qualification", sys.argv[1])
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
catalog, root = Path(sys.argv[2]), Path(sys.argv[3])
original = json.loads(catalog.read_text())
selection = module.native_catalog_selection(catalog, root)
assert len(selection["source_files"]) == 5
assert {Path(file["path"]).suffix for file in selection["source_files"]} == {".hs", ".hs-boot", ".lhs", ".lhs-boot"}
for legacy in (False, True):
    altered = json.loads(json.dumps(original))
    files = altered["source_selection"]["source_files"]
    if legacy:
        altered["source_selection"]["source_files"] = [[file["path"], file["sha256"]] for file in files]
    else:
        files[0]["sha256"] = ("0" if files[0]["sha256"][0] != "0" else "1") + files[0]["sha256"][1:]
    catalog.write_text(json.dumps(altered))
    try:
        module.native_catalog_selection(catalog, root)
    except ValueError:
        pass
    else:
        raise AssertionError("corrupted or legacy source witness was accepted")
catalog.write_text(json.dumps(original))
(root / "jev/core/Jev.lhs-boot").write_text("changed original bytes")
try:
    module.native_catalog_selection(catalog, root)
except ValueError:
    pass
else:
    raise AssertionError("changed literate boot source was accepted")
"#,
            )
            .arg(qualification)
            .arg(catalog)
            .arg(root)
            .output()
            .expect("execute declared Python qualification consumer");
        assert!(
            output.status.success(),
            "Rust/Python source selection disagreement: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl NativeCatalogSourceSelection {
    /// Inspect catalog source bytes using the catalog's SHA-256 witness contract.
    pub fn source_manifest(
        snapshot_root: &Path,
    ) -> Result<Vec<NativeCatalogSourceFile>, crate::cache::SourceManifestError> {
        Ok(crate::cache::catalog_source_sha256_manifest(snapshot_root)?
            .into_iter()
            .map(|(path, sha256)| NativeCatalogSourceFile { path, sha256 })
            .collect())
    }

    pub(crate) fn capture(snapshot_root: &Path) -> Result<Self, ModulePackageError> {
        Self::capture_under(snapshot_root, RootPolicy::NixStore)
    }

    pub(super) fn capture_under(
        snapshot_root: &Path,
        policy: RootPolicy,
    ) -> Result<Self, ModulePackageError> {
        require_immutable_roots(policy, snapshot_root)?;
        if !snapshot_root.is_dir() || absolute(snapshot_root).as_deref() != Some(snapshot_root) {
            return Err(ModulePackageError::RootMoved);
        }
        reject_source_aliases(snapshot_root)?;
        for role in NativeSourceRole::ORDERED {
            let root = snapshot_root.join(role.relative_root());
            if !root.is_dir() || absolute(&root).as_ref() != Some(&root) {
                return Err(ModulePackageError::Format("native source role directory"));
            }
        }
        if !snapshot_root.join("TidepoolCatalog.hs").is_file() {
            return Err(ModulePackageError::Format("native catalog probe"));
        }
        Ok(Self {
            snapshot_root: snapshot_root.to_owned(),
            roles: NativeSourceRole::ORDERED,
            source_files: Self::source_manifest(snapshot_root)
                .map_err(|error| io(&error.path, error.source))?,
        })
    }

    pub(super) fn validate_under(&self, policy: RootPolicy) -> Result<(), ModulePackageError> {
        if self.roles != NativeSourceRole::ORDERED {
            return Err(ModulePackageError::Format("native source role order"));
        }
        let current = Self::capture_under(&self.snapshot_root, policy)?;
        if current.source_files != self.source_files {
            return Err(ModulePackageError::SourceChanged);
        }
        Ok(())
    }

    pub fn root(&self, role: NativeSourceRole) -> PathBuf {
        self.snapshot_root.join(role.relative_root())
    }

    pub fn include_roots(&self) -> Vec<PathBuf> {
        self.roles.iter().map(|role| self.root(*role)).collect()
    }

    pub fn contains_source(&self, path: &Path) -> bool {
        path.is_absolute()
            && !path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
            && self
                .include_roots()
                .iter()
                .any(|root| path.starts_with(root))
    }
}
