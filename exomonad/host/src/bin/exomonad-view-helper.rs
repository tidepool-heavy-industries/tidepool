//! Enter one retained filesystem view before executing one command.

#[cfg(target_os = "linux")]
fn main() {
    exomonad_node::view_command::helper_main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("view command helper requires Linux");
    std::process::exit(1);
}
