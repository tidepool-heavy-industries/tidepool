//! Qualified value-name grammar shared by lookup and the shipped usage index.

/// Split a dotted, lowercase-final `Name` query into its qualifier prefix and
/// final identifier: the shape a qualified value or field has (`Cmd.exitCode`,
/// `R.await`), as opposed to the dotted-capitalized shape already classified
/// `Qualified` (`Cmd.RunResult`) or a bare name with no dot at all. `None` when
/// the query has no dot, either side is empty, the qualifier segments are not
/// each capitalized-alias shaped, or the final identifier is not itself a
/// plain lowercase-led Haskell identifier.
pub(crate) fn qualifier_and_identifier(name: &str) -> Option<(&str, &str)> {
    let (qualifier, identifier) = name.rsplit_once('.')?;
    if qualifier.is_empty() || identifier.is_empty() {
        return None;
    }
    let mut identifier_chars = identifier.chars();
    match identifier_chars.next() {
        Some(first) if first.is_ascii_lowercase() || first == '_' => {}
        _ => return None,
    }
    if !identifier_chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'') {
        return None;
    }
    let qualifier_shaped = qualifier.split('.').all(|segment| {
        let mut chars = segment.chars();
        matches!(chars.next(), Some(first) if first.is_ascii_uppercase())
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'')
    });
    qualifier_shaped.then_some((qualifier, identifier))
}
