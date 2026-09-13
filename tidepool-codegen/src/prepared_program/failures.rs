//! Prepared failure values: authenticated message storage and deliberate machine
//! disposition, independent of GHC's exception heap representation.

use crate::host_fns::RuntimeError;
use tidepool_repr::execution_schema::WiredInErrorKind;

/// Decode only a complete NUL-terminated span owned by the compiled program.
/// This does not dereference the numeric address or force a Haskell value.
pub(super) fn wired_in_failure(
    bytes: &super::static_bytes::PinnedBytes,
    kind: WiredInErrorKind,
    address: usize,
) -> Result<RuntimeError, RuntimeError> {
    if kind == WiredInErrorKind::AbsentSumField {
        return Ok(RuntimeError::WiredInError {
            kind,
            message: "entered absent sum field!".into(),
        });
    }
    let length = bytes.c_string_len(address).ok_or(RuntimeError::BadPointer)?;
    let raw = bytes.read_range(address, length).ok_or(RuntimeError::BadPointer)?;
    let text = String::from_utf8_lossy(raw);
    let untangle = |message: &str| {
        let (location, details) = match text.split_once('|') {
            Some((location, details)) => (location, format!(" {details}")),
            None => (text.as_ref(), String::new()),
        };
        format!("{location}: {message}{details}\n")
    };
    use WiredInErrorKind::*;
    let message = match kind {
        PatternMatch => untangle("Non-exhaustive patterns in"),
        NonExhaustiveGuards => untangle("Non-exhaustive guards in"),
        RecordConstruction => untangle("Missing field in record construction"),
        NoMethodBinding => untangle("No instance nor default method for class operation"),
        RecordSelector => format!("No match in record selector {text}"),
        _ => text.into_owned(),
    };
    Ok(match kind {
        PatternMatch | NonExhaustiveGuards => RuntimeError::PatternMatchFailure(message),
        _ => RuntimeError::WiredInError { kind, message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Arc};

    #[test]
    fn wired_message_uses_owned_utf8_and_ghc_untangle() {
        let payload: Arc<[u8]> = Arc::from(&b"Suite.hs:3|f\0"[..]);
        let address = payload.as_ptr() as usize;
        let pool = super::super::static_bytes::PinnedBytes::new(BTreeMap::from([
            (b"Suite.hs:3|f".to_vec(), payload),
        ]));
        assert_eq!(wired_in_failure(&pool, WiredInErrorKind::PatternMatch, address),
            Ok(RuntimeError::PatternMatchFailure("Suite.hs:3: Non-exhaustive patterns in f\n".into())));
        assert_eq!(wired_in_failure(&pool, WiredInErrorKind::PatternMatch, 0),
            Err(RuntimeError::BadPointer));
    }
}
