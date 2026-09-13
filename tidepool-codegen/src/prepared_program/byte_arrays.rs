//! W5_BYTE_ARRAY: use the descriptor/ledger admission in arrays::active_payload.
//! All host operations are noncollecting. New wrappers are reserved and have
//! valid headers before allocating external storage; no raw payload crosses GC.
//! Word8 indices count bytes; Int indices count target words (eight bytes).

use crate::host_fns::RuntimeError;

#[derive(Clone, Copy)]
pub(super) enum Element {
    Word8,
    Int64,
}

impl Element {
    pub(super) fn bytes(self) -> usize {
        match self { Self::Word8 => 1, Self::Int64 => 8 }
    }
}

#[derive(Clone, Copy)]
pub(super) enum ByteOperation {
    New,
    Freeze,
    Size,
    Read(Element),
    Write(Element),
}

/// Prove the complete element fits before computing or dereferencing its raw
/// address. Diagnostics count elements, matching GHC's index convention.
fn checked_offset(index: i64, byte_len: usize, element: Element) -> Result<usize, RuntimeError> {
    let width = element.bytes();
    let len = byte_len / width;
    let index = usize::try_from(index).ok().filter(|index| *index < len)
        .ok_or(RuntimeError::ArrayIndexOutOfBounds { index, len })?;
    // index < byte_len / width proves the multiplication and full span fit.
    Ok(index * width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn w5_byte_array_bounds_use_element_width_without_overflow() {
        assert_eq!(checked_offset(1, 16, Element::Int64), Ok(8));
        assert_eq!(checked_offset(15, 16, Element::Word8), Ok(15));
        assert!(matches!(checked_offset(1, 15, Element::Int64),
            Err(RuntimeError::ArrayIndexOutOfBounds { index: 1, len: 1 })));
        assert!(checked_offset(-1, usize::MAX, Element::Word8).is_err());
        assert!(checked_offset(i64::MAX, usize::MAX, Element::Int64).is_err());
    }
}
