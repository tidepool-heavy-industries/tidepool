use super::{LinkError, LinkedProgram, MachineImports, PreparedProgram};

/// Atomically link a validated program against one immutable import snapshot.
///
/// No machine import may be published or consumed until every declared import
/// has passed identity, signature, evaluatedness and generation checks.
pub fn link_program(
    prepared: PreparedProgram,
    imports: &MachineImports,
) -> Result<LinkedProgram, LinkError> {
    let mut resolved = Vec::with_capacity(prepared.globals().len());
    for declaration in prepared.globals() {
        let imported = imports
            .values
            .get(&declaration.identity)
            .ok_or_else(|| LinkError::MissingImport(declaration.identity.clone()))?;
        let wrong_entry = declaration.entry_signature.is_some_and(|signature| {
            imported.entry_signature.as_ref() != Some(&prepared.signatures()[signature.0 as usize])
        });
        if imported.identity != declaration.identity
            || imported.rep != declaration.rep
            || wrong_entry
            || (imported.rep != super::RuntimeRep::LiftedRef && imported.entry_signature.is_some())
            || (declaration.required_evaluated && !imported.evaluated)
            || declaration
                .required_generation
                .is_some_and(|generation| imported.generation != generation)
        {
            return Err(LinkError::ImportContract(declaration.identity.clone()));
        }
        resolved.push(imported.clone());
    }
    Ok(super::linked_from_validated(prepared, resolved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_schema::{
        Architecture, Endianness, GlobalDecl, Group, HeapBinding, HeapRhs, ImportedValue,
        ProgramEnvelope, RuntimeRep, Signature, SignatureId, SymbolIdentity, TargetDescriptor,
        TopBinding, UpdatePolicy, ValueId, WireProgram, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
    };

    fn identity() -> SymbolIdentity {
        SymbolIdentity {
            unit: "fixture".into(),
            module: "M3.Import".into(),
            namespace: "value".into(),
            occurrence: "retained".into(),
        }
    }

    fn prepared(required_generation: Option<u64>) -> PreparedProgram {
        super::super::prepared_from_validated(WireProgram {
            envelope: ProgramEnvelope {
                schema_version: SCHEMA_VERSION,
                projection_profile: "ghc-9.12-prepared-stg".into(),
                toolchain: "ghc-9.12.2".into(),
                execution_abi_version: EXECUTION_ABI_VERSION,
                target: TargetDescriptor {
                    architecture: Architecture::X86_64,
                    endianness: Endianness::Little,
                    pointer_width: 64,
                    word_width: 64,
                    abi: "sysv64".into(),
                    features: vec![],
                },
            },
            signatures: vec![Signature {
                arguments: vec![],
                results: vec![RuntimeRep::LiftedRef],
            }],
            globals: vec![GlobalDecl {
                identity: identity(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: Some(SignatureId(0)),
                required_evaluated: true,
                required_generation,
            }],
            constructors: vec![],
            operations: vec![],
            bindings: vec![Group::NonRecursive(TopBinding {
                identity: SymbolIdentity {
                    occurrence: "entry".into(),
                    ..identity()
                },
                binding: HeapBinding {
                    id: ValueId(0),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(0),
                        update: UpdatePolicy::Memoize,
                        captures: vec![],
                        body: Box::new(crate::execution_schema::Expr::Return(vec![])),
                    },
                },
            })],
            entry: ValueId(0),
        })
    }

    fn imports(generation: u64) -> MachineImports {
        let value = ImportedValue {
            identity: identity(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(Signature {
                arguments: vec![],
                results: vec![RuntimeRep::LiftedRef],
            }),
            evaluated: true,
            generation,
        };
        MachineImports {
            values: [(value.identity.clone(), value)].into_iter().collect(),
        }
    }

    #[test]
    fn links_exact_retained_generation_atomically() {
        let linked = link_program(prepared(Some(7)), &imports(7)).unwrap();
        assert_eq!(linked.imports()[0].generation, 7);
    }

    #[test]
    fn rejects_stale_same_name_generation() {
        assert!(matches!(
            link_program(prepared(Some(7)), &imports(8)),
            Err(LinkError::ImportContract(_))
        ));
    }

    #[test]
    fn rejects_import_identity_disagreeing_with_snapshot_key() {
        let mut snapshot = imports(7);
        snapshot
            .values
            .get_mut(&identity())
            .unwrap()
            .identity
            .occurrence = "different".into();
        assert!(matches!(
            link_program(prepared(Some(7)), &snapshot),
            Err(LinkError::ImportContract(_))
        ));
    }

    #[test]
    fn static_import_accepts_current_snapshot_generation() {
        assert!(link_program(prepared(None), &imports(99)).is_ok());
    }

    #[test]
    fn rejects_signature_and_evaluatedness_mismatch() {
        let mut wrong_signature = imports(7);
        wrong_signature
            .values
            .get_mut(&identity())
            .unwrap()
            .entry_signature
            .as_mut()
            .unwrap()
            .results = vec![RuntimeRep::Int(64)];
        assert!(matches!(
            link_program(prepared(Some(7)), &wrong_signature),
            Err(LinkError::ImportContract(_))
        ));

        let mut unevaluated = imports(7);
        unevaluated.values.get_mut(&identity()).unwrap().evaluated = false;
        assert!(matches!(
            link_program(prepared(Some(7)), &unevaluated),
            Err(LinkError::ImportContract(_))
        ));
    }

    #[test]
    fn compares_import_signature_shapes_not_local_ids() {
        let mut equal_id_different_shape = imports(7);
        equal_id_different_shape
            .values
            .get_mut(&identity())
            .unwrap()
            .entry_signature
            .as_mut()
            .unwrap()
            .results = vec![RuntimeRep::Int(64)];
        assert!(matches!(
            link_program(prepared(Some(7)), &equal_id_different_shape),
            Err(LinkError::ImportContract(_))
        ));

        let mut different_id_equal_shape = prepared(Some(7));
        different_id_equal_shape.wire.signatures.push(Signature {
            arguments: vec![],
            results: vec![RuntimeRep::LiftedRef],
        });
        different_id_equal_shape.wire.globals[0].entry_signature = Some(SignatureId(1));
        assert!(link_program(different_id_equal_shape, &imports(7)).is_ok());
    }

    #[test]
    fn raw_address_import_has_no_invented_callable_contract() {
        let mut prepared = prepared(None);
        prepared.wire.globals[0].rep = RuntimeRep::Address;
        prepared.wire.globals[0].entry_signature = None;
        let mut imports = imports(7);
        let imported = imports.values.get_mut(&identity()).unwrap();
        imported.rep = RuntimeRep::Address;
        imported.entry_signature = None;
        assert!(link_program(prepared, &imports).is_ok());
    }

    #[test]
    fn unknown_lifted_entry_accepts_known_import_entry() {
        let mut prepared = prepared(None);
        prepared.wire.globals[0].entry_signature = None;
        assert!(link_program(prepared, &imports(7)).is_ok());
    }

    #[test]
    fn rejects_representation_mismatch_and_missing_required_entry() {
        let mut wrong_rep = imports(7);
        wrong_rep.values.get_mut(&identity()).unwrap().rep = RuntimeRep::Address;
        assert!(matches!(
            link_program(prepared(None), &wrong_rep),
            Err(LinkError::ImportContract(_))
        ));

        let mut missing_entry = imports(7);
        missing_entry
            .values
            .get_mut(&identity())
            .unwrap()
            .entry_signature = None;
        assert!(matches!(
            link_program(prepared(None), &missing_entry),
            Err(LinkError::ImportContract(_))
        ));
    }

    #[test]
    fn rejects_missing_import_without_partial_link() {
        assert!(matches!(
            link_program(prepared(Some(7)), &MachineImports::default()),
            Err(LinkError::MissingImport(_))
        ));
    }
}
