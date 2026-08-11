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
    let mut all_libraries = Vec::new();

    for provider in &providers {
        println!("== {} ==", provider.name());

        let report = provider.discover();
        for diagnostic in &report.diagnostics {
            eprintln!("  [{}] {}", diagnostic.stage, diagnostic.message);
        }
        let libraries = report.data;
        {
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
            all_libraries.extend(libraries);
        }
        println!();
    }

    println!(
        "TOTAL across all vendors: {} librar(y/ies), {} game(s).",
        grand_total_libraries, grand_total_games
    );

    // Per-provider output above is NOT what the app registers: `worker::scan`
    // merges libraries that share a root and then drops games (and the
    // inferred libraries left holding none) already claimed by an earlier
    // provider. Printing only the raw per-provider view is how a phantom
    // library - an EA "library" that was really the inside of a Steam one -
    // reached the UI unnoticed, so the final list is printed too. This is the
    // section to compare against the app's library panel.
    let merged = providers::merge_libraries_by_path(all_libraries);
    let final_libraries = providers::dedupe_games_across_libraries(merged);

    println!();
    println!("== AFTER MERGE + CROSS-PROVIDER DEDUPE (what the app registers) ==");
    for library in &final_libraries {
        println!(
            "  [{}] {} ({} game(s))",
            library.vendor,
            library.path.display(),
            library.games.len()
        );
    }
    let final_games: usize = final_libraries.iter().map(|lib| lib.games.len()).sum();
    println!(
        "  FINAL: {} librar(y/ies), {} game(s) - {} librar(y/ies) and {} game(s) \
         collapsed as duplicates.",
        final_libraries.len(),
        final_games,
        grand_total_libraries - final_libraries.len(),
        grand_total_games - final_games
    );
}
