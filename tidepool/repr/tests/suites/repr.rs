#[path = "../execution_schema_codec.rs"]
mod execution_schema_codec;
#[path = "../execution_schema_contract.rs"]
mod execution_schema_contract;
#[path = "../extend_checked_equivalence.rs"]
mod extend_checked_equivalence;
#[path = "../metadata_strictness.rs"]
mod metadata_strictness;
#[cfg(target_os = "linux")]
#[path = "../strict_jsonl_directory.rs"]
mod strict_jsonl_directory;
