//! Restore public declaration authority through the existing log and scope owners.

use super::*;
use std::collections::BTreeSet;
use std::sync::Arc;
use tidepool_toolchain::declaration_join::{
    certify_recovered_declaration_tip, ClassInstanceEvidence, DeclarationExport, DeclarationKind,
    ExportIdentity, ExportNamespace, InstanceInventory, RecoveryDeclarationSelection,
};

impl SessionLib {
    pub(super) fn hydrate_recovery_graph(
        &self,
        graph: &recovery::RecoveryGraph,
        recovery_root: &Path,
    ) -> Result<DeclLog, SessionError> {
        let invalid = |detail: String| SessionError::RecoveryManifest {
            path: recovery_root.to_path_buf(),
            detail,
        };
        let mut log = DeclLog::new();
        if !log.restore_high_water(graph.high_water) {
            return Err(invalid(
                "could not restore burned declaration identities".into(),
            ));
        }
        for surface in &graph.public_surfaces {
            let Some(generation) = surface.declaration_root else {
                continue;
            };
            if log.recovered_at(generation).is_some() {
                continue;
            }
            let node = graph
                .nodes
                .iter()
                .find(|node| node.id == generation)
                .ok_or_else(|| invalid("recovered public root is absent".into()))?;
            if node.lexical_roots.len() != 1 {
                return Err(invalid(
                    "recovered declaration tip has no unique lexical root".into(),
                ));
            }
            let root = node.lexical_roots[0].clone();
            let projection = graph
                .projection(&surface.owner, &BTreeMap::new())
                .map_err(|error| invalid(error.to_string()))?;
            let mut exports = Vec::new();
            for head in projection.values() {
                let recovery::RecoveryHead::Available { export, .. } = head else {
                    return Err(invalid(
                        "public declaration root retains unavailable live state".into(),
                    ));
                };
                exports.push(recovered_export(export).ok_or_else(|| {
                    invalid("unsupported durable declaration export identity".into())
                })?);
            }
            if !matches!(
                node.state,
                recovery::RecoveryNodeState::ExactArtifactClosure
            ) || !node.live_dependencies.is_empty()
            {
                return Err(invalid(
                    "public declaration root retains unavailable live state".into(),
                ));
            }
            let selected = graph
                .artifacts
                .iter()
                .filter(|artifact| node.artifact_refs.contains(&artifact.key()))
                .collect::<Vec<_>>();
            let mut products = Vec::new();
            let mut joins = Vec::new();
            for artifact in selected {
                match artifact {
                    recovery::RecoveryArtifactClosure::Home(reference) => {
                        products.push(reference.clone())
                    }
                    recovery::RecoveryArtifactClosure::Join(reference) => {
                        joins.push(reference.clone())
                    }
                }
            }
            let instances = recovered_instances(&node.instances)
                .ok_or_else(|| invalid("unsupported durable instance identity".into()))?;
            let family_closure = node
                .instances
                .family_consistency_closure
                .iter()
                .map(recovered_identity)
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| invalid("unsupported durable family closure identity".into()))?;
            let evidence = Arc::new(certify_recovered_declaration_tip(
                recovery_root,
                &products,
                &joins,
                RecoveryDeclarationSelection {
                    root: root.clone(),
                    lexical: node.lexical.clone(),
                    exports,
                    instances,
                    family_closure,
                },
                &self.extra_include,
            )?);
            let source_roots = node
                .lexical
                .iter()
                .find(|lexical| lexical.owner == root)
                .ok_or_else(|| invalid("recovered root lacks lexical edges".into()))?
                .imports
                .clone();
            let mut reachable = BTreeSet::new();
            let mut pending = source_roots.clone();
            while let Some(owner) = pending.pop() {
                if reachable.insert(owner.clone()) {
                    let lexical = node
                        .lexical
                        .iter()
                        .find(|lexical| lexical.owner == owner)
                        .ok_or_else(|| invalid("recovered lexical surface is not closed".into()))?;
                    pending.extend(lexical.imports.clone());
                }
            }
            let source_surface = render::AdmittedDeclarationSurface {
                roots: source_roots,
                lexical: node
                    .lexical
                    .iter()
                    .filter(|lexical| reachable.contains(&lexical.owner))
                    .cloned()
                    .collect(),
            };
            let imports = graph
                .workbench_imports(generation)
                .map_err(|error| invalid(error.to_string()))?;
            let turn = render::DeclTurn {
                normalized: DeclarationSource {
                    prologue: Default::default(),
                    body: String::new(),
                },
                external_imports: SourceImports::new(),
                sources: Vec::new(),
                workbench_imports: SourceImports::from_specs(imports),
                items: evidence.exports().iter().map(recovered_item).collect(),
                value_types: BTreeMap::new(),
                retracts: Vec::new(),
                parent: None,
            };
            if !log.restore_recovered(
                generation,
                render::RecoveredDeclaration {
                    turn,
                    evidence,
                    surface: source_surface,
                },
            ) {
                return Err(invalid(
                    "recovered tip conflicts with its burned module identity".into(),
                ));
            }
        }
        Ok(log)
    }
}

fn recovered_identity(identity: &recovery::RecoverySymbolIdentity) -> Option<ExportIdentity> {
    let namespace = match identity.namespace.as_str() {
        "value" => ExportNamespace::Value,
        "type" => ExportNamespace::Type,
        "constructor" => ExportNamespace::Constructor,
        "field" => ExportNamespace::Field,
        _ => return None,
    };
    let record_parent = match &identity.record_parent {
        None => None,
        Some(parent)
            if parent.unit == identity.unit
                && parent.module == identity.module
                && parent.namespace == "type"
                && parent.record_parent.is_none() =>
        {
            Some(parent.occurrence.clone())
        }
        Some(_) => return None,
    };
    Some(ExportIdentity {
        unit: identity.unit.clone(),
        module: identity.module.clone(),
        namespace,
        occurrence: identity.occurrence.clone(),
        record_parent,
    })
}

fn recovered_export(export: &recovery::RecoveryExport) -> Option<DeclarationExport> {
    Some(DeclarationExport {
        kind: match export.kind {
            recovery::RecoveryExportKind::Value => DeclarationKind::Value,
            recovery::RecoveryExportKind::Type => DeclarationKind::Type,
            recovery::RecoveryExportKind::Class => DeclarationKind::Class,
        },
        head: recovered_identity(&export.identity)?,
        children: export
            .children
            .iter()
            .map(recovered_identity)
            .collect::<Option<_>>()?,
    })
}

fn recovered_instances(
    inventory: &recovery::RecoveryInstanceInventory,
) -> Option<InstanceInventory> {
    Some(InstanceInventory {
        classes: inventory
            .classes
            .iter()
            .filter(|instance| instance.selected)
            .map(|instance| {
                Some(ClassInstanceEvidence {
                    dfun: recovered_identity(&instance.dfun)?,
                    class: recovered_identity(&instance.class)?,
                    selected_axioms: instance
                        .selected_axioms
                        .iter()
                        .map(recovered_identity)
                        .collect::<Option<_>>()?,
                })
            })
            .collect::<Option<_>>()?,
        families: inventory
            .selected_family_axioms
            .iter()
            .map(recovered_identity)
            .collect::<Option<_>>()?,
    })
}

fn recovered_item(export: &DeclarationExport) -> ExportItem {
    match export.kind {
        DeclarationKind::Value => ExportItem::Value {
            name: export.head.occurrence.clone(),
        },
        DeclarationKind::Type => ExportItem::Type {
            name: export.head.occurrence.clone(),
            cons: export
                .children
                .iter()
                .map(|child| child.occurrence.clone())
                .collect(),
        },
        DeclarationKind::Class => ExportItem::Class {
            name: export.head.occurrence.clone(),
            methods: export
                .children
                .iter()
                .map(|child| child.occurrence.clone())
                .collect(),
        },
    }
}
