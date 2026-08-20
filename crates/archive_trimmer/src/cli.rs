//! CLI Argument Definitions and Command Execution.

use clap::{Args, Parser, Subcommand};
use std::fs::File;
use std::path::PathBuf;

use crate::formats::{FormatDetector, TrimOptions};
use crate::scanner;
use crate::sparse;

#[derive(Parser, Debug)]
#[command(
    name = "archive-trimmer",
    author = "GameTrimmer Contributors",
    version = "1.0.0",
    about = "Conservative monolithic game archive inspector. Mutation is disabled pending full payload rollback."
)]
pub struct Cli {
    #[arg(short, long, global = true, help = "Output results in JSON format")]
    pub json: bool,

    #[arg(short, long, global = true, help = "Enable verbose output")]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(about = "Launch interactive desktop graphical user interface")]
    Gui(GuiArgs),

    #[command(about = "Scan candidate archives directly from GameTrimmer SQLite database")]
    ScanDb(ScanDbArgs),

    #[command(about = "Scan a game directory for trimmable monolithic archives")]
    Scan(ScanArgs),

    #[command(about = "Inspect and analyze a single game archive in detail")]
    Analyze(AnalyzeArgs),

    #[command(about = "Validate a single-archive trim request (mutation currently disabled)")]
    Trim(TrimArgs),

    #[command(about = "Validate a batch trim request (mutation currently disabled)")]
    BatchTrim(BatchTrimArgs),

    #[command(about = "Verify NTFS sparse ranges and physical allocated size for an archive")]
    VerifySparse(VerifySparseArgs),
}

#[derive(Args, Debug)]
pub struct GuiArgs {
    #[arg(long, help = "Path to gametrimmer.db (auto-detected if omitted)")]
    pub db: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ScanDbArgs {
    #[arg(long, help = "Path to gametrimmer.db (auto-detected if omitted)")]
    pub db: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ScanArgs {
    #[arg(help = "Path to the game installation directory")]
    pub path: PathBuf,

    #[arg(long, help = "Maximum search depth")]
    pub max_depth: Option<usize>,
}

#[derive(Args, Debug)]
pub struct AnalyzeArgs {
    #[arg(help = "Path to the archive file (.pck, .pak, .asar, .bik, .bundle)")]
    pub archive_path: PathBuf,
}

#[derive(Args, Debug)]
pub struct TrimArgs {
    #[arg(help = "Path to the archive file to trim")]
    pub archive_path: PathBuf,

    #[arg(
        long,
        default_value = "english,sfx,common",
        value_delimiter = ',',
        help = "Comma-separated list of language codes/names to keep"
    )]
    pub keep_languages: Vec<String>,

    #[arg(long, help = "Simulate trimming without modifying files")]
    pub dry_run: bool,

    #[arg(long, help = "Disable automatic .gt_snap header snapshot creation")]
    pub no_snapshot: bool,

    #[arg(long, hide = true)]
    pub force_unsafe: bool,

    #[arg(long, help = "Custom directory to store header snapshots")]
    pub backup_dir: Option<PathBuf>,

    #[arg(
        long,
        help = "Game installation root used for the mandatory anti-cheat scan"
    )]
    pub game_dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct BatchTrimArgs {
    #[arg(help = "Path to the game installation directory")]
    pub game_dir: PathBuf,

    #[arg(
        long,
        default_value = "english,sfx,common",
        value_delimiter = ',',
        help = "Comma-separated list of language codes/names to keep"
    )]
    pub keep_languages: Vec<String>,

    #[arg(long, help = "Simulate trimming without modifying files")]
    pub dry_run: bool,

    #[arg(long, help = "Disable automatic .gt_snap header snapshot creation")]
    pub no_snapshot: bool,

    #[arg(long, hide = true)]
    pub force_unsafe: bool,

    #[arg(long, help = "Custom directory to store header snapshots")]
    pub backup_dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct VerifySparseArgs {
    #[arg(help = "Path to the archive file to verify")]
    pub archive_path: PathBuf,
}

/// Executes the CLI command.
pub fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Gui(args) => handle_gui(args),
        Commands::ScanDb(args) => handle_scan_db(args, cli.json, cli.verbose),
        Commands::Scan(args) => handle_scan(args, cli.json, cli.verbose),
        Commands::Analyze(args) => handle_analyze(args, cli.json, cli.verbose),
        Commands::Trim(args) => handle_trim(args, cli.json, cli.verbose),
        Commands::BatchTrim(args) => handle_batch_trim(args, cli.json, cli.verbose),
        Commands::VerifySparse(args) => handle_verify_sparse(args, cli.json),
    }
}

fn handle_gui(args: GuiArgs) -> Result<(), Box<dyn std::error::Error>> {
    crate::gui::run_gui(args.db)?;
    Ok(())
}

fn handle_scan_db(
    args: ScanDbArgs,
    json: bool,
    _verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = args
        .db
        .or_else(crate::db_reader::find_default_db_path)
        .ok_or("Could not locate gametrimmer.db. Specify --db <path> explicitly.")?;

    let candidates = crate::db_reader::read_games_with_candidates(&db_path)?;

    let mut scanned_games = Vec::new();
    let mut total_logical_size = 0u64;
    let mut total_on_disk_size = 0u64;
    let mut total_potential_savings = 0u64;
    let mut total_archives_count = 0usize;

    for game in candidates {
        let game_root = game.game_root();
        let safety_report =
            crate::anti_cheat::check_game_safety(&game_root, false).unwrap_or_default();
        let mut detected_archives = Vec::new();
        let mut game_logical_size = 0u64;
        let mut game_on_disk_size = 0u64;
        let mut game_trimmable_bytes = 0u64;
        let mut game_languages = Vec::new();

        for file in &game.candidate_files {
            if !file.full_path.exists() {
                continue;
            }
            if let Ok(Some(archive_type)) = FormatDetector::detect_file(&file.full_path) {
                let handler = FormatDetector::get_handler(archive_type);
                if let Ok(analysis) = handler.analyze(&file.full_path) {
                    if analysis.total_trimmable_bytes > 0
                        && analysis.trimmable_chunks.iter().any(|c| c.is_language)
                    {
                        game_logical_size = game_logical_size.saturating_add(analysis.total_size);
                        game_on_disk_size = game_on_disk_size.saturating_add(analysis.on_disk_size);
                        game_trimmable_bytes =
                            game_trimmable_bytes.saturating_add(analysis.total_trimmable_bytes);

                        for lang in &analysis.detected_languages {
                            if !game_languages.contains(lang) {
                                game_languages.push(lang.clone());
                            }
                        }

                        detected_archives.push(analysis);
                    }
                }
            }
        }

        if !detected_archives.is_empty() {
            total_archives_count += detected_archives.len();
            total_logical_size = total_logical_size.saturating_add(game_logical_size);
            total_on_disk_size = total_on_disk_size.saturating_add(game_on_disk_size);
            total_potential_savings = total_potential_savings.saturating_add(game_trimmable_bytes);

            game_languages.sort();

            scanned_games.push(serde_json::json!({
                "game_id": game.game_id,
                "game_name": game.game_name,
                "game_root": game_root,
                "is_safe": safety_report.is_safe,
                "safety_findings": safety_report.findings,
                "archives_count": detected_archives.len(),
                "logical_size": game_logical_size,
                "on_disk_size": game_on_disk_size,
                "trimmable_bytes": game_trimmable_bytes,
                "detected_languages": game_languages,
                "archives": detected_archives,
            }));
        }
    }

    if json {
        let output = serde_json::json!({
            "database_path": db_path,
            "total_games_with_archives": scanned_games.len(),
            "total_archives": total_archives_count,
            "total_logical_size": total_logical_size,
            "total_on_disk_size": total_on_disk_size,
            "total_potential_savings": total_potential_savings,
            "games": scanned_games,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("============================================================");
        println!("GameTrimmer Database Archive Scanner: {:?}", db_path);
        println!("============================================================");
        println!("Games with Monoliths: {}", scanned_games.len());
        println!("Total Archives:       {}", total_archives_count);
        println!("Total Logical Size:   {}", format_bytes(total_logical_size));
        println!("Total On-Disk Size:   {}", format_bytes(total_on_disk_size));
        println!(
            "Potential Savings:    {}",
            format_bytes(total_potential_savings)
        );
        println!("------------------------------------------------------------");

        for (i, game_val) in scanned_games.iter().enumerate() {
            let name = game_val["game_name"].as_str().unwrap_or("Unknown");
            let is_safe = game_val["is_safe"].as_bool().unwrap_or(true);
            let count = game_val["archives_count"].as_u64().unwrap_or(0);
            let trimmable = game_val["trimmable_bytes"].as_u64().unwrap_or(0);

            println!(
                "[{:02}] {} | {} | {} archives | Potential Savings: {}",
                i + 1,
                name,
                if is_safe {
                    "[SAFE]"
                } else {
                    "[ANTI-CHEAT PROTECTED]"
                },
                count,
                format_bytes(trimmable)
            );
        }
    }

    Ok(())
}

fn handle_scan(
    args: ScanArgs,
    json: bool,
    _verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = scanner::scan_game_directory(&args.path, args.max_depth)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("============================================================");
        println!("GameTrimmer Archive Scanner: {:?}", report.game_root);
        println!("============================================================");
        println!(
            "Anti-Cheat Safety: {}",
            if report.is_safe {
                "[SAFE] No anti-cheat detected"
            } else {
                "[BLOCKED] Anti-cheat detected!"
            }
        );
        if !report.is_safe {
            for finding in &report.safety_report.findings {
                println!("  - {}: {:?}", finding.engine, finding.detected_files);
            }
        }
        println!("Archives Found:     {}", report.archives_scanned);
        println!(
            "Total Logical Size: {}",
            format_bytes(report.total_logical_size)
        );
        println!(
            "Current On-Disk:    {}",
            format_bytes(report.total_on_disk_size)
        );
        println!(
            "Potential Savings:  {}",
            format_bytes(report.total_potential_savings)
        );
        println!(
            "Detected Languages: {}",
            report.all_detected_languages.join(", ")
        );
        println!("------------------------------------------------------------");

        for (i, arch) in report.detected_archives.iter().enumerate() {
            let rel = arch.path.strip_prefix(&args.path).unwrap_or(&arch.path);
            println!("[{}] {:?} ({})", i + 1, rel, arch.archive_type);
            println!(
                "    Size: {} | Trimmable: {} | Chunks: {}",
                format_bytes(arch.total_size),
                format_bytes(arch.total_trimmable_bytes),
                arch.trimmable_chunks.len()
            );
            if !arch.detected_languages.is_empty() {
                println!("    Languages: {}", arch.detected_languages.join(", "));
            }
        }
    }

    Ok(())
}

fn handle_analyze(
    args: AnalyzeArgs,
    json: bool,
    _verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let archive_type = FormatDetector::detect_file(&args.archive_path)?.ok_or_else(|| {
        format!(
            "Unknown or unsupported archive format for {:?}",
            args.archive_path
        )
    })?;

    let handler = FormatDetector::get_handler(archive_type);
    let analysis = handler.analyze(&args.archive_path)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&analysis)?);
    } else {
        println!("============================================================");
        println!("Archive Analysis: {:?}", analysis.path);
        println!("Type:             {}", analysis.archive_type);
        println!("Details:          {}", analysis.details);
        println!("Logical Size:     {}", format_bytes(analysis.total_size));
        println!("On-Disk Size:     {}", format_bytes(analysis.on_disk_size));
        println!(
            "Potential Savings:{}",
            format_bytes(analysis.total_trimmable_bytes)
        );
        println!(
            "Languages:        {}",
            analysis.detected_languages.join(", ")
        );
        println!("------------------------------------------------------------");
        println!("Chunks (first 25 shown):");

        for (i, chunk) in analysis.trimmable_chunks.iter().take(25).enumerate() {
            println!(
                "  [{:03}] Offset: 0x{:08X} | Len: {:>10} | [{:>12}] | {}",
                i + 1,
                chunk.offset,
                format_bytes(chunk.length),
                chunk.language.as_deref().unwrap_or("-"),
                chunk.name
            );
        }
        if analysis.trimmable_chunks.len() > 25 {
            println!(
                "  ... and {} more chunks.",
                analysis.trimmable_chunks.len() - 25
            );
        }
    }

    Ok(())
}

fn handle_trim(
    args: TrimArgs,
    json: bool,
    _verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.force_unsafe {
        return Err(
            "--force-unsafe is no longer supported; anti-cheat safety is a hard block".into(),
        );
    }

    if !args.dry_run {
        let game_dir = args.game_dir.as_deref().ok_or(
            "Live single-file trim requires --game-dir so anti-cheat protection can be checked",
        )?;
        let canonical_game_dir = game_dir.canonicalize()?;
        let canonical_archive = args.archive_path.canonicalize()?;
        if !canonical_archive.starts_with(&canonical_game_dir) {
            return Err(format!(
                "Archive {:?} is outside the declared game directory {:?}",
                args.archive_path, game_dir
            )
            .into());
        }
        let _ = crate::anti_cheat::check_game_safety(&canonical_game_dir, true)?;
    }

    let archive_type = FormatDetector::detect_file(&args.archive_path)?.ok_or_else(|| {
        format!(
            "Unknown or unsupported archive format for {:?}",
            args.archive_path
        )
    })?;

    let handler = FormatDetector::get_handler(archive_type);
    let options = TrimOptions {
        keep_languages: args.keep_languages,
        dry_run: args.dry_run,
        create_snapshot: !args.no_snapshot,
        force_unsafe: false,
        custom_backup_dir: args.backup_dir,
    };

    let result = handler.trim(&args.archive_path, &options)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("============================================================");
        println!("Trimming Complete: {:?}", result.archive_path);
        println!(
            "Mode:              {}",
            if result.is_dry_run {
                "DRY-RUN (Simulated)"
            } else {
                "LIVE IN-PLACE"
            }
        );
        println!("Chunks Zeroed:     {}", result.chunks_trimmed);
        println!(
            "Logical Trimmed:   {}",
            format_bytes(result.logical_bytes_trimmed)
        );
        println!(
            "Physical Freed:    {}",
            format_bytes(result.physical_bytes_freed)
        );
        println!(
            "New On-Disk Size:  {}",
            format_bytes(result.new_on_disk_size)
        );
        if let Some(ref snap) = result.snapshot_path {
            println!("Header Snapshot:   {:?}", snap);
        }
        println!("============================================================");
    }

    Ok(())
}

fn handle_batch_trim(
    args: BatchTrimArgs,
    json: bool,
    _verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.force_unsafe {
        return Err(
            "--force-unsafe is no longer supported; anti-cheat safety is a hard block".into(),
        );
    }

    let options = TrimOptions {
        keep_languages: args.keep_languages,
        dry_run: args.dry_run,
        create_snapshot: !args.no_snapshot,
        force_unsafe: false,
        custom_backup_dir: args.backup_dir,
    };

    let report = scanner::batch_trim_game(&args.game_dir, &options)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("============================================================");
        println!("Batch Trim Summary: {:?}", report.game_root);
        println!("Archives Processed: {}", report.results.len());
        println!("Total Chunks:       {}", report.total_chunks_trimmed);
        println!(
            "Logical Trimmed:    {}",
            format_bytes(report.total_logical_bytes_trimmed)
        );
        println!(
            "Physical Freed:     {}",
            format_bytes(report.total_physical_bytes_freed)
        );
        println!("Snapshots Created:  {}", report.snapshots_created.len());
        println!("============================================================");
    }

    Ok(())
}

fn handle_verify_sparse(
    args: VerifySparseArgs,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::open(&args.archive_path)?;
    let logical_size = file.metadata()?.len();
    let on_disk_size = sparse::get_on_disk_size(&args.archive_path).unwrap_or(logical_size);
    let is_sp = sparse::is_sparse(&args.archive_path).unwrap_or(false);
    let cluster_size = sparse::get_cluster_size(&args.archive_path);
    let ranges = sparse::query_allocated_ranges(&file, 0, logical_size).unwrap_or_default();

    if json {
        let res = serde_json::json!({
            "archive_path": args.archive_path,
            "is_sparse": is_sp,
            "logical_size": logical_size,
            "on_disk_size": on_disk_size,
            "cluster_size": cluster_size,
            "allocated_ranges": ranges,
        });
        println!("{}", serde_json::to_string_pretty(&res)?);
    } else {
        println!("============================================================");
        println!("Sparse Verification: {:?}", args.archive_path);
        println!(
            "Sparse Attribute:    {}",
            if is_sp { "ENABLED" } else { "DISABLED" }
        );
        println!("Cluster Size:        {} bytes", cluster_size);
        println!("Logical Size:        {}", format_bytes(logical_size));
        println!("Physical On-Disk:    {}", format_bytes(on_disk_size));
        println!("Allocated Extents:   {}", ranges.len());
        for (i, (off, len)) in ranges.iter().take(20).enumerate() {
            println!(
                "  Extent #{:02}: [0x{:08X} .. 0x{:08X}] ({} bytes)",
                i + 1,
                off,
                off + len,
                len
            );
        }
        if ranges.len() > 20 {
            println!("  ... and {} more extents.", ranges.len() - 20);
        }
        println!("============================================================");
    }

    Ok(())
}

/// Helper function to format byte counts into readable strings (KiB, MiB, GiB).
pub fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;

    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}
