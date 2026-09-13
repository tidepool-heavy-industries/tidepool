//! Shared authored fixtures for execution-schema tests.
//!
//! These helpers construct schema values directly so tests can alter typed
//! fields and still exercise the normal validation and decoding boundaries.

use super::{
    Architecture, Atom, Endianness, ExprFrame, Group, HeapBinding, HeapRhs, ProgramEnvelope,
    RuntimeRep, ScalarLiteral, Signature, SignatureId, SymbolIdentity, TargetDescriptor,
    DecodeLimits, ParseError, PreparedProgram, ProgramRequirements, TopBinding, ValueId,
    WireProgram, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};

/// A stable identity for authored execution-schema fixtures.
pub fn identity(module: &str, occurrence: &str) -> SymbolIdentity {
    SymbolIdentity {
        unit: "fixture".into(),
        module: module.into(),
        namespace: "value".into(),
        occurrence: occurrence.into(),
        record_parent: None,
    }
}

/// The supported target used by execution-schema fixtures.
pub fn target() -> TargetDescriptor {
    TargetDescriptor {
        architecture: Architecture::X86_64,
        endianness: Endianness::Little,
        pointer_width: 64,
        word_width: 64,
        abi: "sysv64".into(),
        features: vec![],
    }
}

/// The matching producer envelope for [`wire_program`].
pub fn envelope() -> ProgramEnvelope {
    ProgramEnvelope {
        schema_version: SCHEMA_VERSION,
        projection_profile: "ghc-9.12-prepared-stg".into(),
        toolchain: "ghc-9.12.2".into(),
        execution_abi_version: EXECUTION_ABI_VERSION,
        target: target(),
    }
}

/// Build a minimal closed program accepted by the execution-schema validator.
pub fn wire_program() -> WireProgram {
    WireProgram {
        envelope: envelope(),
        signatures: vec![Signature {
            arguments: vec![],
            results: vec![RuntimeRep::Int(64)],
        }],
        globals: vec![],
        constructors: vec![],
        operations: vec![],
        expressions: super::Expr {
            nodes: vec![ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 42_i64.to_be_bytes().to_vec(),
            })])],
        },
        bindings: vec![Group::NonRecursive(TopBinding {
            identity: identity("Fixture", "entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Function {
                    signature: SignatureId(0),
                    parameters: vec![],
                    captures: vec![],
                    body: 0,
                },
            },
        })],
        entry: ValueId(0),
    }
}

/// Validate and publish an authored fixture through the normal preparation
/// boundary. No unchecked `PreparedProgram` construction is exposed.
pub fn prepare(wire: WireProgram) -> Result<PreparedProgram, ParseError> {
    let envelope = wire.envelope.clone();
    let requirements = ProgramRequirements {
        schema_version: envelope.schema_version,
        projection_profile: envelope.projection_profile,
        toolchain: envelope.toolchain,
        execution_abi_version: envelope.execution_abi_version,
        target: envelope.target,
    };
    super::validation::validate_program(&wire, &requirements, DecodeLimits::default())?;
    Ok(super::prepared_from_validated(wire))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn baseline_wire_program_validates() {
        prepare(wire_program()).unwrap();
    }
}
