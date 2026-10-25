use super::*;
use crate::execution_schema::testing;

fn symbol(unit: &str, module: &str, namespace: &str, occurrence: &str) -> SymbolIdentity {
    SymbolIdentity {
        unit: unit.into(),
        module: module.into(),
        namespace: namespace.into(),
        occurrence: occurrence.into(),
        record_parent: None,
    }
}

fn local(id: u32) -> Atom {
    Atom::Ref(ValueRef::Local(ValueId(id)))
}

fn fixture() -> WireProgram {
    let mut wire = testing::wire_program();
    wire.signatures.extend([
        Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        },
        Signature {
            arguments: vec![RuntimeRep::LiftedRef; 2],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        },
        Signature {
            arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::Int(64)],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        },
        Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Word(8)]),
        },
    ]);
    wire.globals = vec![
        GlobalDecl {
            identity: testing::identity("User", "unused"),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: Some(42),
        },
        GlobalDecl {
            identity: symbol(
                "tidepool-resume-hash",
                "Tidepool.Internal.Resume",
                "value",
                "settle",
            ),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(1)),
            required_evaluated: true,
            required_generation: None,
        },
        GlobalDecl {
            identity: symbol(
                "tidepool-resume-hash",
                "Tidepool.Internal.Resume",
                "value",
                "resumeLifted",
            ),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(2)),
            required_evaluated: true,
            required_generation: None,
        },
    ];
    let int = ConstructorDecl {
        identity: ConstructorIdentity {
            symbol: symbol("ghc-prim", "GHC.Types", "constructor", "I#"),
            family: TypeIdentity {
                symbol: symbol("ghc-prim", "GHC.Types", "type", "Int"),
                version: TypeVersion::WiredIn,
            },
        },
        host_id: crate::DataConId(1001),
        result_rep: RuntimeRep::LiftedRef,
        field_reps: vec![RuntimeRep::Int(64)],
        strict_fields: vec![true],
        layout: CheckedLayout {
            fields: vec![FieldLayout {
                rep: RuntimeRep::Int(64),
                offset: 0,
            }],
            alignment: 8,
            payload_size: 8,
            root_mask: vec![false],
        },
        tag: 1,
        family_size: 1,
    };
    let mut unused = int.clone();
    unused.identity.symbol = symbol("user", "User", "constructor", "Unused");
    unused.identity.family = testing::type_identity(symbol("user", "User", "type", "Unused"));
    unused.host_id = crate::DataConId(1002);
    wire.constructors = vec![unused, int];
    for (ordinal, name) in HELPERS.iter().enumerate() {
        let first = 20 + ordinal as u32 * 10;
        let boxed = first + 2;
        let binder = first + 3;
        let call = wire.expressions.nodes.len();
        wire.expressions.nodes.push(if *name == "__resume" {
            ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Global(GlobalId(2))),
                signature: SignatureId(2),
                arguments: vec![local(first), local(first + 1)],
            }
        } else {
            ExprFrame::Call {
                callee: local(first),
                signature: SignatureId(1),
                arguments: vec![local(if *name == "__applyEntry" {
                    boxed
                } else {
                    first + 1
                })],
            }
        });
        let settle = wire.expressions.nodes.len();
        wire.expressions.nodes.push(ExprFrame::Call {
            callee: Atom::Ref(ValueRef::Global(GlobalId(1))),
            signature: SignatureId(1),
            arguments: vec![local(binder)],
        });
        let body = wire.expressions.nodes.len();
        wire.expressions.nodes.push(ExprFrame::Case {
            scrutinee: call,
            binder: ValueId(binder),
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            kind: CaseKind::Algebraic(testing::type_identity(symbol(
                "freer-simple-hash",
                "Control.Monad.Freer.Internal",
                "type",
                "Eff",
            ))),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: settle,
            }],
        });
        let body = if *name == "__applyEntry" {
            let outer = wire.expressions.nodes.len();
            wire.expressions.nodes.push(ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(boxed),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(1),
                        fields: vec![local(first + 1)],
                    },
                }),
                body,
            });
            outer
        } else {
            body
        };
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Fixture", name),
            binding: HeapBinding {
                id: ValueId(10 + ordinal as u32),
                rhs: HeapRhs::Function {
                    signature: SignatureId(if *name == "__applyEntry" { 3 } else { 2 }),
                    parameters: vec![ValueId(first), ValueId(first + 1)],
                    captures: vec![],
                    body,
                },
            },
        }));
    }
    wire
}

#[test]
fn compacts_actual_bodies_and_preserves_exact_imports_and_constructor() {
    let wire = fixture();
    let source = testing::prepare(wire.clone()).unwrap();
    let compact = source.extract_apply_support().unwrap().unwrap();
    assert_eq!(compact.bindings().len(), 3);
    assert_eq!(compact.globals().len(), 2);
    assert_eq!(compact.constructors(), &wire.constructors[1..]);
    assert_eq!(compact.signatures(), &wire.signatures[1..4]);
    assert_eq!(compact.globals()[0].identity, wire.globals[1].identity);
    assert_eq!(compact.globals()[0].entry_signature, Some(SignatureId(0)));
    assert_eq!(compact.globals()[1].entry_signature, Some(SignatureId(1)));
    assert_eq!(
        compact.expressions().nodes.len() + 1,
        wire.expressions.nodes.len()
    );
    assert!(compact.operations().is_empty());
    assert!(compact.sites().is_empty());
    assert!(compact.constructor_replies().is_empty());
    assert_eq!(compact.types(), &Arc::default());
    assert!(compact.json_layout().is_none());
    for (projected, original) in tops(compact.bindings()).zip(tops(&wire.bindings).skip(1)) {
        assert_eq!(projected.identity, original.identity);
        assert_eq!(projected.binding.id, original.binding.id);
    }
    let encoded = super::super::encoding::encode_wire_program(compact.wire.as_ref());
    let e = compact.envelope();
    let requirements = ProgramRequirements {
        schema_version: e.schema_version,
        projection_profile: e.projection_profile.clone(),
        toolchain: e.toolchain.clone(),
        execution_abi_version: e.execution_abi_version,
        target: e.target.clone(),
    };
    assert_eq!(
        parse_program(&encoded, &requirements, DecodeLimits::default()).unwrap(),
        compact
    );
    assert_eq!(compact.extract_apply_support().unwrap().unwrap(), compact);
}

#[test]
fn absent_or_foreign_owner_helpers_do_not_issue_support() {
    assert!(testing::prepare(testing::wire_program())
        .unwrap()
        .extract_apply_support()
        .unwrap()
        .is_none());
    let mut wire = fixture();
    let Group::NonRecursive(top) = &mut wire.bindings[3] else {
        unreachable!()
    };
    top.identity.module = "OtherOwner".into();
    assert!(testing::prepare(wire)
        .unwrap()
        .extract_apply_support()
        .unwrap()
        .is_none());
}

#[test]
fn rejects_user_global_retained_generation_and_competing_package() {
    for change in 0..3 {
        let mut wire = fixture();
        match change {
            0 => wire.globals[1].identity = testing::identity("User", "settle"),
            1 => wire.globals[1].required_generation = Some(7),
            _ => wire.globals[1].identity.unit = "tidepool-resume-other".into(),
        }
        let source = testing::prepare(wire).unwrap();
        assert!(matches!(
            source.extract_apply_support(),
            Err(ParseError::InvalidReference(_))
        ));
    }
}

#[test]
fn full_validation_refuses_dependency_on_removed_user_top() {
    let mut wire = fixture();
    // The original user entry has the same one-argument lifted function contract,
    // so the original prepared program is valid while the reduced one is open.
    wire.signatures[0] = wire.signatures[1].clone();
    let Group::NonRecursive(top) = &mut wire.bindings[0] else {
        unreachable!()
    };
    let HeapRhs::Function { parameters, .. } = &mut top.binding.rhs else {
        unreachable!()
    };
    *parameters = vec![ValueId(99)];
    wire.expressions.nodes[0] = ExprFrame::Return(vec![local(99)]);
    if let ExprFrame::Call { callee, .. } = &mut wire.expressions.nodes[4] {
        *callee = local(0);
    } else {
        unreachable!()
    }
    let source = testing::prepare(wire).unwrap();
    assert!(matches!(
        source.extract_apply_support(),
        Err(ParseError::InvalidScope(_))
    ));
}

#[test]
fn rejects_reachable_user_constructor_and_literal_but_drops_unrelated_data() {
    let mut wire = fixture();
    let last = wire
        .expressions
        .nodes
        .iter_mut()
        .find_map(|frame| {
            if let ExprFrame::Let {
                bindings: Group::NonRecursive(binding),
                ..
            } = frame
            {
                Some(binding)
            } else {
                None
            }
        })
        .unwrap();
    let HeapRhs::Constructor { constructor, .. } = &mut last.rhs else {
        unreachable!()
    };
    *constructor = ConstructorId(0);
    assert!(testing::prepare(wire)
        .unwrap()
        .extract_apply_support()
        .is_err());
    let mut wire = fixture();
    for frame in &mut wire.expressions.nodes {
        if let ExprFrame::Let {
            bindings: Group::NonRecursive(binding),
            ..
        } = frame
        {
            let HeapRhs::Constructor { fields, .. } = &mut binding.rhs else {
                unreachable!()
            };
            fields[0] = Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 17_i64.to_be_bytes().to_vec(),
            });
        }
    }
    assert!(testing::prepare(wire)
        .unwrap()
        .extract_apply_support()
        .is_err());
    let mut source = fixture();
    let expected = testing::prepare(source.clone())
        .unwrap()
        .extract_apply_support()
        .unwrap();
    source.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
        bits: 64,
        bytes: (-99_i64).to_be_bytes().to_vec(),
    })]);
    assert_eq!(
        testing::prepare(source)
            .unwrap()
            .extract_apply_support()
            .unwrap(),
        expected
    );
}
