//! Cranelift compiler and runtime machinery for prepared-STG programs.
//!
//! This crate validates prepared execution schemas, emits native code, and
//! owns heap, collection, rooting, cancellation, and continuation mechanics.

pub mod alloc;
pub mod binding_table;
pub mod context;
pub mod debug;
pub mod descriptor_bridge;
pub mod entry_abi;
pub mod gc;
pub mod host_fns;
pub mod layout;
pub mod machine;
pub mod machine_state;
pub mod observation;
pub mod old_space;
pub mod pipeline;
pub mod prepared_control;
pub mod prepared_program;
mod resource_ledger;
pub mod scope;
pub mod stack_map;
pub mod suspension;
