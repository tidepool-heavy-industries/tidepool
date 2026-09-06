// Each module remains a separate source file; nextest isolates each test process.
#[path = "../stdlib_regressions_02.rs"]
mod stdlib_regressions_02;
#[path = "../stdlib_regressions_02_medium.rs"]
mod stdlib_regressions_02_medium;
#[path = "../test_error_msg.rs"]
mod test_error_msg;
#[path = "../text_filter_gc.rs"]
mod text_filter_gc;
#[path = "../validator_reject.rs"]
mod validator_reject;
#[path = "../vendor_text_functions.rs"]
mod vendor_text_functions;
#[path = "../numeric_oracle.rs"]
mod numeric_oracle;
