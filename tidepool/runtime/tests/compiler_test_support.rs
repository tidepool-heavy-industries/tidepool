//! Shared ownership controls for genuine compiler integration cases.
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;

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
