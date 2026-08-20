//! Monolithic Game Archive Inspector CLI & GUI (`archive-trimmer` / `gametrimmer-archive`).

use clap::Parser;

fn main() {
    // If run without arguments, launch GUI mode
    if std::env::args().len() <= 1 {
        if let Err(err) = archive_trimmer::gui::run_gui(None) {
            eprintln!("GUI Error: {}", err);
            std::process::exit(1);
        }
        return;
    }

    let cli = archive_trimmer::cli::Cli::parse();

    if let Err(err) = archive_trimmer::cli::run(cli) {
        eprintln!("Error: {}", err);
        std::process::exit(1);
    }
}
