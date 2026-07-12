//! Read-only smoke test for library discovery across all vendors.
//!
//! Run with: `cargo run -p gametrimmer-core --example steam_smoke`
//!
//! Only reads the registry and the filesystem (registry keys, manifest files,
//! and directory listings) - performs no writes, deletes, or renames anywhere,
//! including on any library drive.

use gametrimmer_core::providers;

fn main() {
    let providers = providers::all();

    let mut grand_total_libraries = 0usize;
    let mut grand_total_games = 0usize;

    for provider in &providers {
        println!("== {} ==", provider.name());

        match provider.discover() {
            Ok(libraries) => {
                if libraries.is_empty() {
                    println!("  (no libraries found)");
                    println!();
                    continue;
                }

                for library in &libraries {
                    println!(
                        "  - {} ({} game(s))",
                        library.path.display(),
                        library.games.len()
                    );
                    for game in &library.games {
                        println!(
                            "      [{}] {} -> {}",
                            game.app_id.as_deref().unwrap_or("?"),
                            game.name,
                            game.install_dir.display()
                        );
                    }
                }

                let total_games: usize = libraries.iter().map(|lib| lib.games.len()).sum();
                println!(
                    "  Subtotal: {} librar(y/ies), {} game(s).",
                    libraries.len(),
                    total_games
                );
                grand_total_libraries += libraries.len();
                grand_total_games += total_games;
            }
            Err(err) => {
                eprintln!("  {} discovery failed: {err}", provider.name());
            }
        }
        println!();
    }

    println!(
        "TOTAL across all vendors: {} librar(y/ies), {} game(s).",
        grand_total_libraries, grand_total_games
    );
}
