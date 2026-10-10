# tidepool-repr — prepared execution schemas and metadata

This crate owns the versioned prepared-program wire schema, constructor metadata, shared identifiers, and durable formats. Keep decoding bounded and validation explicit. It does not define or serialize a Tidepool Core IR.


Constructor metadata carries the mandatory compiler-issued `SymbolIdentity`.
Diagnostic qualified names can have several unit owners and never choose one.
Joint admission validates every prepared constructor against complete metadata;
collection before target completion grants no joint admission. See
[the format and producer contract](../../docs/constructor-metadata.md).
