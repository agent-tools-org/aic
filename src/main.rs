//! Binary entry point for the aic CLI.
//! Exports: main; deps: aic library runner.

fn main() -> anyhow::Result<()> {
    aic::run()
}
