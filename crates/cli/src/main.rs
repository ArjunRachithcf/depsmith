//! The `depsmith` executable.
fn main() {
    std::process::exit(depsmith_cli::run_from(std::env::args().collect()).into());
}
