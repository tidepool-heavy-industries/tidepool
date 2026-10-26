//! Shared ownership controls for genuine compiler integration cases.
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use tidepool_toolchain::artifacts::CompiledArtifacts;
use tidepool_toolchain::declaration_join::{ExactLexicalNode, ExactModuleIdentity};

pub(super) struct OwnedEnvironment {
    name: &'static str,
    previous: Option<OsString>,
}

impl OwnedEnvironment {
    pub(super) fn set(name: &'static str, value: Option<&Path>) -> Self {
        let previous = std::env::var_os(name);
        // This test is serial and owns these compiler environment inputs.
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        Self { name, previous }
    }
}

impl Drop for OwnedEnvironment {
    fn drop(&mut self) {
        // Restore the same serial test's original compiler environment.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

/// Preserve the selected compiler graph when retaining a source owner.
pub(super) fn selected_lexical_closure(
    original: &CompiledArtifacts,
    roots: Vec<ExactModuleIdentity>,
) -> BTreeMap<ExactModuleIdentity, ExactLexicalNode> {
    let inventory = original
        .module_inventory
        .as_ref()
        .expect("original compiler graph");
    let mut pending = roots;
    let mut lexical = BTreeMap::new();
    while let Some(selected) = pending.pop() {
        if lexical.contains_key(&selected) {
            continue;
        }
        let module = inventory
            .iter()
            .find(|module| {
                !module.boot && module.unit == selected.unit && module.module == selected.module
            })
            .expect("selected original has consumed compiler evidence");
        let imports = module
            .imports
            .iter()
            .filter_map(|import| {
                let path = import.selected.as_ref()?;
                let imported = inventory
                    .iter()
                    .find(|candidate| {
                        candidate.source == *path
                            && candidate.module == import.module
                            && candidate.boot == import.boot
                    })
                    .expect("selected home import has an exact consumed owner");
                Some(ExactModuleIdentity {
                    unit: imported.unit.clone(),
                    module: imported.module.clone(),
                })
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        pending.extend(imports.iter().cloned());
        lexical.insert(
            selected.clone(),
            ExactLexicalNode {
                owner: selected,
                imports,
            },
        );
    }
    lexical
}

pub(super) fn capture_world_sources(
    includes: &[std::path::PathBuf],
    preamble: &str,
    row: &str,
) -> (
    tidepool_runtime::session::GenerationArtifactReference,
    Arc<tidepool_toolchain::compiler_world::CapturedCompilerSources>,
) {
    use std::io::Read;
    use tidepool_toolchain::compiler_world::{CapturedCompilerSourceRoot, CapturedCompilerSources};
    let owner = Arc::new(tempfile::tempdir().unwrap());
    let mut remaining = CapturedCompilerSources::capture_byte_allowance().unwrap();
    let mut roots = Vec::new();
    for (ordinal, include) in includes.iter().enumerate() {
        remaining = remaining
            .checked_sub(4)
            .expect("compiler source root framing fits capture allowance");
        let files = tidepool_toolchain::cache::source_root_manifest(include).unwrap();
        let destination = owner.path().join(format!("root-{ordinal}"));
        std::fs::create_dir_all(&destination).unwrap();
        let mut bodies = std::collections::BTreeMap::new();
        for (relative, digest) in &files {
            let input = include.join(relative);
            let length = std::fs::metadata(&input).unwrap().len();
            let framed = 8u64
                .checked_add(relative.as_os_str().len() as u64)
                .and_then(|size| size.checked_add(length))
                .unwrap();
            remaining = remaining
                .checked_sub(framed)
                .expect("compiler source bytes fit capture allowance before allocation");
            let mut body = Vec::new();
            std::fs::File::open(&input)
                .unwrap()
                .take(length.checked_add(1).unwrap())
                .read_to_end(&mut body)
                .unwrap();
            assert_eq!(
                body.len() as u64,
                length,
                "compiler source changed during capture"
            );
            assert_eq!(
                blake3::hash(&body).to_hex().as_str(),
                digest,
                "compiler source differs from its observed manifest"
            );
            let target = destination.join(relative);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, &body).unwrap();
            bodies.insert(relative.clone(), Arc::<[u8]>::from(body));
        }
        roots.push(CapturedCompilerSourceRoot {
            manifest: tidepool_toolchain::cache::SourceRootManifest::from_file_digests(files)
                .unwrap(),
            bodies,
        });
    }
    let manifests = roots
        .iter()
        .map(|root| root.manifest.clone())
        .collect::<Vec<_>>();
    let identity = tidepool_toolchain::cache::source_manifests_identity(
        b"integration-world-source",
        &manifests,
    );
    let reference = tidepool_runtime::session::GenerationArtifactReference::capture(
        serde_json::to_vec(&(preamble, row, identity)).unwrap(),
    );
    let captured =
        CapturedCompilerSources::from_captured_roots(roots, Arc::from(reference.bytes()), owner)
            .unwrap();
    (reference, Arc::new(captured))
}

type FixtureSession =
    tidepool_runtime::session::ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>;

pub(super) fn install_world(
    session: &mut FixtureSession,
    source_owner: &Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
    includes: &[std::path::PathBuf],
    preamble: &str,
    row: &str,
) {
    let public = source_owner.scope();
    let (reference, sources) = capture_world_sources(includes, preamble, row);
    let admitted = session
        .capture_inspection_world_in(public, &reference, source_owner.original_source())
        .unwrap();
    let settled = source_owner
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            tidepool_runtime::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, _| {
                tidepool_runtime::session::run_admitted_compiler_world(owner, admitted, || {
                    Ok(Some(sources))
                })
            },
        )
        .unwrap();
    let completed = tidepool_runtime::session::finish_admitted_compiler_world(settled).unwrap();
    assert!(session.install_compiler_world_if_current(
        &completed,
        &reference,
        source_owner.original_source()
    ));
}

pub(super) fn plan_checked_cell(
    session: &mut FixtureSession,
    source_owner: &Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
    execution: Arc<tidepool_runtime::session::PrivateExecutionAdmission>,
    specification: Arc<tidepool_extract_cmd::WorldCellRecipe>,
) -> Arc<tidepool_runtime::session::RuntimeCellAdmission> {
    use tidepool_runtime::session::turn::{
        finish_world_cell_plan_admitted, prepare_world_cell_plan_admitted,
    };
    let settled = session
        .with_source_compiler_transaction(
            source_owner.original_source().clone(),
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            tidepool_runtime::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |session, _| session.prepare_private_execution(execution.clone(), None),
        )
        .unwrap();
    assert!(settled.close().resources_retired());
    let (snapshot, operand) = settled.into_outcome().action.unwrap().into_parts();
    assert!(operand.is_none());
    let settled = source_owner
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            tidepool_runtime::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, _| {
                prepare_world_cell_plan_admitted(owner, snapshot.world(), specification, None)
            },
        )
        .unwrap();
    let plan = finish_world_cell_plan_admitted(settled).unwrap();
    let settled = session
        .with_source_compiler_transaction(
            source_owner.original_source().clone(),
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            tidepool_runtime::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |session, _| {
                session.admit_world_planned_cell_with_snapshot(execution, snapshot, plan, None)
            },
        )
        .unwrap();
    assert!(settled.close().resources_retired());
    settled.into_outcome().action.unwrap()
}
