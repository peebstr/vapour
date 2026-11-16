fn main() {
    if let Err(e) = vapour::cli::run() {
        eprintln!("\x1b[31m[ERROR]\x1b[0m {e}");
        std::process::exit(e.exit_code());
    }
}
