//! Versioned prepared-execution artifact contract shared by the extractor
//! writer, compile cache, and native consumers.

use tidepool_repr::execution_schema::{
    link_program, parse_program, Architecture, DecodeLimits, Endianness, LinkError, LinkedProgram,
    MachineImports, ParseError, PreparedProgram, ProgramRequirements, TargetDescriptor,
    EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};

pub const PREPARED_SUFFIX: &str = ".prepared.cbor";
pub const PROJECTION_PROFILE: &str = "ghc-9.12-prepared-stg";
pub const TOOLCHAIN_VERSION: &str = "ghc-9.12.2";

/// Exact host contract accepted by production artifact readers. Unsupported
/// targets fail before an artifact can be parsed or cached.
pub fn production_requirements() -> Result<ProgramRequirements, ParseError> {
    let architecture = match std::env::consts::ARCH {
        "x86_64" => Architecture::X86_64,
        "aarch64" => Architecture::Aarch64,
        other => {
            return Err(ParseError::UnsupportedTarget(format!(
                "prepared execution is not configured for {other}"
            )))
        }
    };
    let pointer_width = usize::BITS as u8;
    let abi = match &architecture {
        Architecture::X86_64 => "sysv64",
        Architecture::Aarch64 => "aapcs64",
    };
    Ok(ProgramRequirements {
        schema_version: SCHEMA_VERSION,
        projection_profile: PROJECTION_PROFILE.into(),
        toolchain: TOOLCHAIN_VERSION.into(),
        execution_abi_version: EXECUTION_ABI_VERSION,
        target: TargetDescriptor {
            architecture,
            endianness: if cfg!(target_endian = "little") {
                Endianness::Little
            } else {
                Endianness::Big
            },
            pointer_width,
            word_width: pointer_width,
            abi: abi.into(),
            features: vec![],
        },
    })
}

pub fn prepared_artifact_name(target: &str) -> String {
    format!("{target}{PREPARED_SUFFIX}")
}

/// Bytes are retained with their checked projection so cache publication can
/// preserve the exact writer output while consumers cannot bypass parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedArtifact {
    bytes: Vec<u8>,
    prepared: PreparedProgram,
}

impl PreparedArtifact {
    pub fn parse(bytes: Vec<u8>, limits: DecodeLimits) -> Result<Self, ParseError> {
        let requirements = production_requirements()?;
        Self::parse_for_requirements(bytes, &requirements, limits)
    }

    fn parse_for_requirements(
        bytes: Vec<u8>,
        requirements: &ProgramRequirements,
        limits: DecodeLimits,
    ) -> Result<Self, ParseError> {
        let prepared = parse_program(&bytes, requirements, limits)?;
        Ok(Self { bytes, prepared })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn prepared(&self) -> &PreparedProgram {
        &self.prepared
    }

    pub fn link(self, imports: &MachineImports) -> Result<LinkedProgram, LinkError> {
        link_program(self.prepared, imports)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::{ImportedValue, RuntimeRep};

    fn fixture() -> Vec<u8> {
        include_bytes!("../../tidepool-eval/tests/fixtures/m3-vertical.cbor").to_vec()
    }

    fn imports(prepared: &PreparedProgram) -> MachineImports {
        let values = prepared
            .globals()
            .iter()
            .map(|global| {
                let value = ImportedValue {
                    identity: global.identity.clone(),
                    signature: prepared.signatures()[global.signature.0 as usize].clone(),
                    evaluated: global.required_evaluated,
                    generation: global.required_generation.unwrap_or(0),
                };
                (value.identity.clone(), value)
            })
            .collect();
        MachineImports { values }
    }

    #[test]
    fn exact_artifact_parses_and_links_atomically() {
        let artifact = PreparedArtifact::parse(fixture(), DecodeLimits::default()).unwrap();
        let machine_imports = imports(artifact.prepared());
        artifact.link(&machine_imports).unwrap();
    }

    #[test]
    fn malformed_and_profile_mismatched_artifacts_are_rejected() {
        assert!(PreparedArtifact::parse(vec![0xff], DecodeLimits::default()).is_err());

        let mut requirements = production_requirements().unwrap();
        requirements.projection_profile = "stale-profile".into();
        assert!(PreparedArtifact::parse_for_requirements(
            fixture(),
            &requirements,
            DecodeLimits::default()
        )
        .is_err());
    }

    #[test]
    fn mismatched_import_contract_is_rejected() {
        let artifact = PreparedArtifact::parse(fixture(), DecodeLimits::default()).unwrap();
        let mut machine_imports = imports(artifact.prepared());
        let import = machine_imports.values.values_mut().next().unwrap();
        import.signature.arguments.push(RuntimeRep::Int(64));
        assert!(artifact.link(&machine_imports).is_err());
    }
}
