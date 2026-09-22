use super::{DecodeLimits, ParseError, PreparedProgram, ProgramRequirements};

/// Decode and validate one prepared execution artifact.
///
/// The implementation owns all structural, scope, signature, layout and limit
/// checks. A caller can never construct the private success type directly.
pub fn parse_program(
    bytes: &[u8],
    requirements: &ProgramRequirements,
    limits: DecodeLimits,
) -> Result<PreparedProgram, ParseError> {
    let wire = super::codec::decode_wire(bytes, limits)?;
    super::validation::validate_program(&wire, requirements, limits)?;
    Ok(super::prepared_from_validated(wire))
}
