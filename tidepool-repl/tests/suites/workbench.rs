#[path = "../common/mod.rs"]
mod common;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../name_shadowing.rs"]
mod name_shadowing;
#[path = "../repro_decl_library_import.rs"]
mod repro_decl_library_import;
#[path = "../repro_t_multiline_sig.rs"]
mod repro_t_multiline_sig;
#[path = "../session_acceptance.rs"]
mod session_acceptance;
#[path = "../stub_fetch.rs"]
mod stub_fetch;
#[path = "../text_bind.rs"]
mod text_bind;
#[path = "../value_binding_acceptance.rs"]
mod value_binding_acceptance;
#[path = "../workbench_imports.rs"]
mod workbench_imports;
