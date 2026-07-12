//! Read-only smoke test for the full discover -> scan -> classify pipeline,
//! run against one real Steam library.
//!
//! Run with: `cargo run -p gametrimmer-core --example pipeline_smoke -- G:\SteamLibrary`
//! (defaults to `G:\SteamLibrary` when no argument is given.)
//!
//! Only reads the registry, `libraryfolders.vdf`, `appmanifest_*.acf`, and
//! walks the chosen library's game directories - performs no writes,
//! deletes, renames, or database access anywhere on disk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gametrimmer_core::langdetect::{LangDetector, LangFinding, LangKind};
use gametrimmer_core::providers::steam::SteamProvider;
use gametrimmer_core::providers::LibraryProvider;
use gametrimmer_core::rules::{Category, Finding, RuleEngine};
use gametrimmer_core::scanner::scan_dir;

const DEFAULT_LIBRARY: &str = r"G:\SteamLibrary";
const TOP_N: usize = 20;
const TOP_LANG_FINDINGS: usize = 15;
const TOP_LANGUAGES: usize = 10;

struct ClassifiedFile {
    full_path: PathBuf,
    size: u64,
    finding: Finding,
}

struct LangClassifiedFile {
    full_path: PathBuf,
    size: u64,
    finding: LangFinding,
}

fn main() {
    let library_arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_LIBRARY.to_string());
    let library_path = PathBuf::from(&library_arg);

    println!(
        "pipeline_smoke: read-only run against {}",
        library_path.display()
    );

    let rules_path = repo_rules_path();
    let engine = match RuleEngine::load(&rules_path) {
        Ok(engine) => engine,
        Err(err) => {
            eprintln!("Не вдалося завантажити {}: {err}", rules_path.display());
            std::process::exit(1);
        }
    };

    let libraries = match SteamProvider.discover() {
        Ok(libraries) => libraries,
        Err(err) => {
            eprintln!("Помилка пошуку бібліотек Steam: {err}");
            std::process::exit(1);
        }
    };

    let Some(library) = libraries
        .iter()
        .find(|lib| paths_equal(&lib.path, &library_path))
    else {
        eprintln!(
            "Бібліотеку {} не знайдено серед виявлених:",
            library_path.display()
        );
        for lib in &libraries {
            eprintln!("  - {}", lib.path.display());
        }
        std::process::exit(1);
    };

    println!(
        "Знайдено бібліотеку {} з {} грою(ми). Сканування (тільки читання)...\n",
        library.path.display(),
        library.games.len()
    );

    // Default keep-list (uk+en) - see docs/04_implementation_plan.md; a
    // configurable keep-list is a future phase.
    let lang_detector = LangDetector::new();

    let mut classified: Vec<ClassifiedFile> = Vec::new();
    let mut lang_classified: Vec<LangClassifiedFile> = Vec::new();
    let mut scan_errors = 0usize;

    for game in &library.games {
        match scan_dir(&game.install_dir) {
            Ok(entries) => {
                for entry in &entries {
                    if let Some(finding) = engine.classify(&entry.rel_path) {
                        classified.push(ClassifiedFile {
                            full_path: game.install_dir.join(&entry.rel_path),
                            size: entry.size,
                            finding,
                        });
                    }
                }

                // Sibling context (the language-family heuristic) requires
                // analyzing all of a game's files together.
                for (index, finding) in lang_detector.analyze_game(&entries) {
                    let entry = &entries[index];
                    lang_classified.push(LangClassifiedFile {
                        full_path: game.install_dir.join(&entry.rel_path),
                        size: entry.size,
                        finding,
                    });
                }
            }
            Err(err) => {
                scan_errors += 1;
                eprintln!(
                    "Помилка сканування \"{}\" ({}): {err}",
                    game.name,
                    game.install_dir.display()
                );
            }
        }
    }

    print_category_summary(&classified);
    print_top_findings(&classified);
    print_lang_category_summary(&lang_classified);
    print_lang_breakdown(&lang_classified);
    print_top_lang_findings(&lang_classified);

    println!(
        "\nВсього ігор: {}, rules-знахідок: {}, lang-знахідок: {}, помилок сканування: {scan_errors}.",
        library.games.len(),
        classified.len(),
        lang_classified.len()
    );
}

fn print_category_summary(classified: &[ClassifiedFile]) {
    let mut per_category: BTreeMap<&'static str, (usize, u64)> = BTreeMap::new();

    for file in classified {
        let key = category_label(file.finding.category);
        let entry = per_category.entry(key).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += file.size;
    }

    println!("=== Знахідки за категоріями ===");
    if per_category.is_empty() {
        println!("(жодних знахідок)");
    }
    for (label, (count, bytes)) in &per_category {
        println!("  {label}: {count} файл(ів), {}", format_bytes(*bytes));
    }

    let total_bytes: u64 = classified.iter().map(|f| f.size).sum();
    println!(
        "  Разом: {} файл(ів), {}",
        classified.len(),
        format_bytes(total_bytes)
    );
}

fn print_top_findings(classified: &[ClassifiedFile]) {
    let mut sorted: Vec<&ClassifiedFile> = classified.iter().collect();
    sorted.sort_by_key(|file| std::cmp::Reverse(file.size));

    println!("\n=== Топ-{TOP_N} найбільших знахідок ===");
    for (index, file) in sorted.iter().take(TOP_N).enumerate() {
        println!(
            "{:>2}. {} — {} — {}% — {}",
            index + 1,
            file.full_path.display(),
            format_bytes(file.size),
            file.finding.confidence,
            file.finding.rule_desc
        );
    }
}

fn print_lang_category_summary(classified: &[LangClassifiedFile]) {
    let mut per_kind: BTreeMap<&'static str, (usize, u64)> = BTreeMap::new();

    for file in classified {
        let key = lang_kind_label(file.finding.kind);
        let entry = per_kind.entry(key).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += file.size;
    }

    println!("\n=== Знахідки локалізацій за категоріями ===");
    if per_kind.is_empty() {
        println!("(жодних знахідок)");
    }
    for (label, (count, bytes)) in &per_kind {
        println!("  {label}: {count} файл(ів), {}", format_bytes(*bytes));
    }

    let total_bytes: u64 = classified.iter().map(|f| f.size).sum();
    println!(
        "  Разом: {} файл(ів), {}",
        classified.len(),
        format_bytes(total_bytes)
    );
}

fn print_lang_breakdown(classified: &[LangClassifiedFile]) {
    let mut per_lang: BTreeMap<&str, (usize, u64)> = BTreeMap::new();

    for file in classified {
        let entry = per_lang
            .entry(file.finding.lang_tag.as_str())
            .or_insert((0, 0));
        entry.0 += 1;
        entry.1 += file.size;
    }

    let mut sorted: Vec<(&str, usize, u64)> = per_lang
        .into_iter()
        .map(|(lang, (count, bytes))| (lang, count, bytes))
        .collect();
    sorted.sort_by_key(|&(_, _, bytes)| std::cmp::Reverse(bytes));

    println!("\n=== Топ-{TOP_LANGUAGES} мов за сумарним розміром ===");
    if sorted.is_empty() {
        println!("(жодних знахідок)");
    }
    for (index, (lang, count, bytes)) in sorted.iter().take(TOP_LANGUAGES).enumerate() {
        println!(
            "{:>2}. {lang}: {count} файл(ів), {}",
            index + 1,
            format_bytes(*bytes)
        );
    }
}

fn print_top_lang_findings(classified: &[LangClassifiedFile]) {
    let mut sorted: Vec<&LangClassifiedFile> = classified.iter().collect();
    sorted.sort_by_key(|file| std::cmp::Reverse(file.size));

    println!("\n=== Топ-{TOP_LANG_FINDINGS} найбільших lang-знахідок ===");
    for (index, file) in sorted.iter().take(TOP_LANG_FINDINGS).enumerate() {
        println!(
            "{:>2}. {} — {} — {}% — [{}] {} — {}",
            index + 1,
            file.full_path.display(),
            format_bytes(file.size),
            file.finding.confidence,
            file.finding.lang_tag,
            lang_kind_label(file.finding.kind),
            file.finding.reason
        );
    }
}

fn lang_kind_label(kind: LangKind) -> &'static str {
    match kind {
        LangKind::Audio => "loc_audio",
        LangKind::Text => "loc_text",
        LangKind::Video => "loc_video",
        LangKind::Font => "loc_font",
        LangKind::Unknown => "loc_unknown",
    }
}

fn category_label(category: Category) -> &'static str {
    match category {
        Category::RedistFolder => "redist_folder",
        Category::RedistFile => "redist_file",
        Category::DocsFolder => "docs_folder",
        Category::DocsFile => "docs_file",
        Category::Bonus => "bonus",
        Category::DevLeftovers => "dev_leftovers",
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let value = bytes as f64;
    if value >= GB {
        format!("{:.2} ГБ", value / GB)
    } else if value >= MB {
        format!("{:.2} МБ", value / MB)
    } else if value >= KB {
        format!("{:.2} КБ", value / KB)
    } else {
        format!("{bytes} Б")
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

fn repo_rules_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("rules.json")
}
