use super::*;
use crate::prepared_program::RunOptions;
use std::sync::Weak;
use tidepool_repr::execution_schema::{testing, *};

const REUSED: &[u8] = b"portable-literal";
const NEW: &[u8] = b"image-only-literal";

fn linked(wire: WireProgram) -> LinkedProgram {
    link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap()
}

fn byte_program(module: &str, unrelated: Option<Vec<u8>>) -> LinkedProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
    wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]);
    for (index, bytes) in std::iter::once(REUSED.to_vec())
        .chain(unrelated)
        .enumerate()
    {
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity(module, &format!("bytes{index}")),
            binding: HeapBinding {
                id: ValueId(index as u32 + 1),
                rhs: HeapRhs::Bytes(bytes),
            },
        }));
    }
    linked(wire)
}

fn reader_program() -> LinkedProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Word(64)]);
    wire.signatures.push(Signature {
        arguments: vec![RuntimeRep::Address, RuntimeRep::Int(64)],
        results: ResultContract::Returns(vec![RuntimeRep::Word(64)]),
    });
    wire.operations.push(OperationDecl {
        identity: OperationIdentity::PrimOp("indexCharOffAddr#".into()),
        signature: SignatureId(1),
    });
    wire.expressions.nodes[0] = ExprFrame::Operation {
        operation: OperationId(0),
        arguments: vec![
            Atom::Scalar(ScalarLiteral::Bytes(REUSED.to_vec())),
            Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: vec![0; 8],
            }),
        ],
    };
    wire.bindings.push(Group::NonRecursive(TopBinding {
        identity: testing::identity("PortableReader", "newBytes"),
        binding: HeapBinding {
            id: ValueId(1),
            rhs: HeapRhs::Bytes(NEW.to_vec()),
        },
    }));
    linked(wire)
}

fn options() -> PreparedMachineOptions {
    PreparedMachineOptions {
        nursery_bytes: RunOptions::default().nursery_bytes,
    }
}

fn source_and_image() -> (
    PreparedMachine<'static>,
    Arc<CompiledProgram>,
    Weak<[u8]>,
    usize,
) {
    let unrelated = vec![0xa5; 1 << 20];
    let (mut source, _) = PreparedMachine::new(
        CompiledProgram::compile(&byte_program("SourceLiterals", Some(unrelated.clone()))).unwrap(),
        options(),
    )
    .unwrap();
    let pool = source.machine.interned_bytes();
    let unrelated_owner = Arc::downgrade(pool.get(&unrelated).unwrap());
    let reused_address = pool.get(REUSED).unwrap().as_ptr() as usize;
    drop(pool);

    let image = Arc::new(source.compile_for_install(&reader_program()).unwrap());
    assert_eq!(
        image.bytes.get(REUSED).unwrap().as_ptr() as usize,
        reused_address
    );
    assert!(image.bytes.get(NEW).is_some());
    assert!(
        image.bytes.get(&unrelated).is_none(),
        "image owns only used literals"
    );
    (source, image, unrelated_owner, reused_address)
}

fn receiver() -> PreparedMachine<'static> {
    PreparedMachine::new(
        CompiledProgram::compile(&byte_program("ReceiverLiterals", None)).unwrap(),
        options(),
    )
    .unwrap()
    .0
}

fn assert_installed_reader(
    receiver: &mut PreparedMachine<'_>,
    id: ProgramId,
    image: &CompiledProgram,
    reused_address: usize,
) {
    let fresh_address = image.bytes.get(NEW).unwrap().as_ptr() as usize;
    let pool = receiver.machine.interned_bytes();
    assert_ne!(pool.get(REUSED).unwrap().as_ptr() as usize, reused_address);
    assert_eq!(pool.read_range(reused_address, REUSED.len()), Some(REUSED));
    assert_eq!(pool.read_range(fresh_address, NEW.len()), Some(NEW));
    drop(pool);
    let result = receiver
        .run_entry(
            id,
            ValueId(0),
            &[],
            PreparedCallOptions {
                observation_budget: RunOptions::default().observation_budget,
                collect_before_observation: true,
            },
            RealmId::ROOT,
        )
        .unwrap();
    assert!(matches!(result.values.as_slice(),
        [HaskellValue::Lit(tidepool_repr::Literal::LitWord(value))] if *value == u64::from(REUSED[0])));
}

#[test]
fn shared_image_admits_reused_and_new_literal_addresses_without_retaining_source_pool() {
    let (source, image, unrelated_owner, reused_address) = source_and_image();
    drop(source);
    assert!(unrelated_owner.upgrade().is_none());
    let mut receiver = receiver();
    let id = receiver
        .install_shared(Arc::clone(&image), ImportBindings::new())
        .unwrap();
    assert_installed_reader(&mut receiver, id, &image, reused_address);
    drop(image);

    let token = receiver.quiesce().unwrap();
    let retired = receiver.collect_major(token).unwrap();
    assert!(retired.programs.contains(&id));
    assert_eq!(
        receiver.machine.resolve_literal_bytes(|pool| pool
            .read_range(reused_address, REUSED.len())
            .map(<[u8]>::to_vec)),
        Some(REUSED.to_vec()),
        "escaped addresses outlive the image on the destination machine"
    );
}

#[test]
fn parcel_image_admits_reused_literal_after_source_machine_drop() {
    let (mut source, image, unrelated_owner, reused_address) = source_and_image();
    let source_id = source
        .install_shared(Arc::clone(&image), ImportBindings::new())
        .unwrap();
    let entry = source.retain_top(source_id, ValueId(0)).unwrap();
    let parcel = source.export_parcel(entry).unwrap();
    assert_eq!(parcel.images().len(), 1);
    drop(source);
    assert!(unrelated_owner.upgrade().is_none());

    let mut receiver = receiver();
    let crate::prepared_program::ImportedParcel {
        value: arrived,
        imports,
        ..
    } = receiver.import_parcel(parcel, RealmId::ROOT).unwrap();
    assert!(imports.is_empty());
    let id = receiver
        .programs
        .iter()
        .find_map(|(&id, installed)| {
            std::ptr::eq(installed.program.get(), image.as_ref()).then_some(id)
        })
        .unwrap();
    assert_installed_reader(&mut receiver, id, &image, reused_address);
    assert!(receiver.release(arrived));
}
