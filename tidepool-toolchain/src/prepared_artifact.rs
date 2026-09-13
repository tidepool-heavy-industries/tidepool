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
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err(ParseError::UnsupportedTarget(format!(
            "prepared execution requires Linux x86_64; host is {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )));
    }
    Ok(ProgramRequirements {
        schema_version: SCHEMA_VERSION,
        projection_profile: PROJECTION_PROFILE.into(),
        toolchain: TOOLCHAIN_VERSION.into(),
        execution_abi_version: EXECUTION_ABI_VERSION,
        target: TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
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

    #[expect(
        clippy::result_large_err,
        reason = "structured link evidence is retained on this cold artifact-admission path"
    )]
    pub fn link(self, imports: &MachineImports) -> Result<LinkedProgram, LinkError> {
        link_program(self.prepared, imports)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::{ImportedValue, RuntimeRep};

    fn fixture() -> Vec<u8> {
        include_bytes!("../../haskell/test-prepared-stg/fixtures/m3-vertical.cbor").to_vec()
    }

    fn imports(prepared: &PreparedProgram) -> MachineImports {
        let values = prepared
            .globals()
            .iter()
            .map(|global| {
                let value = ImportedValue {
                    identity: global.identity.clone(),
                    rep: global.rep.clone(),
                    entry_signature: global
                        .entry_signature
                        .map(|id| prepared.signatures()[id.0 as usize].clone()),
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
        let import = machine_imports
            .values
            .values_mut()
            .find(|import| import.entry_signature.is_some())
            .expect("fixture must declare a callable import");
        import
            .entry_signature
            .as_mut()
            .unwrap()
            .arguments
            .push(RuntimeRep::Int(64));
        assert!(artifact.link(&machine_imports).is_err());
    }
}
