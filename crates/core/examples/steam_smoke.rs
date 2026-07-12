//! Read-only smoke test for Steam library discovery.
//!
//! Run with: `cargo run -p gametrimmer-core --example steam_smoke`
//!
//! Only reads the registry and the filesystem (registry keys, `libraryfolders.vdf`,
//! `appmanifest_*.acf`, and directory listings) - performs no writes, deletes, or
//! renames anywhere, including on Steam library drives.

use gametrimmer_core::providers::steam::SteamProvider;
use gametrimmer_core::providers::LibraryProvider;

fn main() {
    let provider = SteamProvider;

    match provider.discover() {
        Ok(libraries) => {
            if libraries.is_empty() {
                println!("No Steam libraries found.");
                return;
            }

            println!("Found {} Steam librar(y/ies):", libraries.len());
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
                "\nTotal: {} librar(y/ies), {} game(s).",
                libraries.len(),
                total_games
            );
        }
        Err(err) => {
            eprintln!("Steam discovery failed: {err}");
        }
    }
}
