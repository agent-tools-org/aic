//! Binary entry point for the aic CLI.
//! Exports: main; deps: aic library runner.

fn main() {
    if let Err(error) = aic::run() {
        eprintln!("Error: {error:?}");
        std::process::exit(4);
    }
}
