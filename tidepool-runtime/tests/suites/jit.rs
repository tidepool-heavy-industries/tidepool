// Each module remains a separate source file; nextest isolates each test process.
#[path = "../harness_profile_generic_surface.rs"]
mod harness_profile_generic_surface;
#[path = "../jit_surface.rs"]
mod jit_surface;
#[path = "../multi_module_datacon.rs"]
mod multi_module_datacon;
#[path = "../nested_mapm_tag255.rs"]
mod nested_mapm_tag255;
#[path = "../nullary_sum_generic_deriving.rs"]
mod nullary_sum_generic_deriving;
#[path = "../patch_crosscheck_differential.rs"]
mod patch_crosscheck_differential;
#[path = "../realm_varid_pinning.rs"]
mod realm_varid_pinning;
#[path = "../resident_session.rs"]
mod resident_session;
