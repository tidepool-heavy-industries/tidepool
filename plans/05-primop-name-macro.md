# Plan: Unify PrimOpKind name mapping (Display, encode, decode)

## Goal

The mapping between `PrimOpKind` variants and their string names is maintained in three separate places that must stay in sync. Unify to a single source of truth.

## Current State

| Location | Function | Lines |
|----------|----------|-------|
| `tidepool-repr/src/types.rs` | `Display for PrimOpKind` | 300-511 |
| `tidepool-repr/src/serial/read.rs` | `decode_primop` | 636-849 |
| `tidepool-repr/src/serial/write.rs` | `encode_primop` | 197-409 |

Each has ~150 match arms mapping variant ↔ string. The Display names differ from encode/decode names (Display uses operators like `+#`, encode/decode uses `"IntAdd"`), so this is actually TWO mappings:
- **Serialization name**: `IntAdd`, `IntSub`, etc. (used in CBOR encode/decode)
- **Display name**: `+#`, `-#`, etc. (used in pretty-printing)

## Plan

### Option A: Declarative macro (recommended)

Define a single macro in `tidepool-repr/src/types.rs` that generates the enum, Display impl, and encode/decode functions from one table:

```rust
macro_rules! define_primops {
    ( $( $variant:ident => $serial:literal, $display:literal; )* ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum PrimOpKind {
            $( $variant, )*
        }

        impl std::fmt::Display for PrimOpKind {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let s = match self {
                    $( PrimOpKind::$variant => $display, )*
                };
                write!(f, "{}", s)
            }
        }

        impl PrimOpKind {
            pub fn serial_name(&self) -> &'static str {
                match self {
                    $( PrimOpKind::$variant => $serial, )*
                }
            }

            pub fn from_serial_name(s: &str) -> Option<Self> {
                match s {
                    $( $serial => Some(PrimOpKind::$variant), )*
                    _ => None,
                }
            }
        }
    };
}

define_primops! {
    IntAdd => "IntAdd", "+#";
    IntSub => "IntSub", "-#";
    IntMul => "IntMul", "*#";
    // ... all ~150 variants
}
```

Then `encode_primop` becomes `op.serial_name()` and `decode_primop` becomes `PrimOpKind::from_serial_name(s)`.

### Steps

1. Define `define_primops!` macro in `tidepool-repr/src/types.rs`
2. Replace the existing enum definition (lines 26-248) and Display impl (lines 300-511) with a single macro invocation
3. Add `serial_name()` and `from_serial_name()` methods to PrimOpKind
4. In `serial/write.rs`: replace `encode_primop` body (lines 197-409) with `op.serial_name()`
5. In `serial/read.rs`: replace `decode_primop` body (lines 636-849) with `PrimOpKind::from_serial_name(s).ok_or_else(|| ...)`
6. Verify: the existing `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]` must still be present (put inside macro)

## Verify

```bash
cargo check -p tidepool-repr
cargo test -p tidepool-repr
cargo check --workspace
```

## Boundary

- Do NOT change any variant names or string mappings — purely structural refactor
- Do NOT change the public API — `PrimOpKind` stays the same type
- The macro must preserve all existing derives (Debug, Clone, Copy, PartialEq, Eq, Hash)
- Keep the existing error type/message in decode_primop's failure path
