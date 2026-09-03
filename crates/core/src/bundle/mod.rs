//! The diagnostic bundle: one `.zip` a user reviews and attaches to a bug
//! report themselves.
//!
//! There is no transport here and there is not meant to be. The app never
//! uploads anything; the delivery step is the user picking where to save
//! the file. That is not a limitation working around a missing feature - it
//! is the reason this design needs no consent gate, no endpoint to keep
//! alive, and no network code to audit.
//!
//! # Shape
//!
//! A zip, Deflate at the crate default, with no level setting. Measured on
//! a real 1 598-game library: the default bundle is 83 KB with Deflate and
//! 64 KB with LZMA2, a difference that is invisible on a file attached to
//! an issue - while Explorer's built-in handler reads Deflate and nothing
//! else. A zip a recipient cannot double-click is the wrong failure for a
//! support file, so the recipient's cost decided this, not the ratio.
//!
//! Sections that are excluded are **absent files, not empty ones**, so the
//! manifest's `sections_included` is always exactly the archive's own
//! listing - checkable with `unzip -l` by someone who does not trust it.
//!
//! # Privacy
//!
//! One pass, applied last, over the serialized text of every section - see
//! [`redact`]. Beyond it: no machine GUID, no volume serial, no content
//! hash, no stable per-installation id. The manifest carries a
//! per-generation UUID instead, which solves the only real case (a user
//! producing a "before" and "after" pair in one session) without letting
//! two files posted months apart be linked as the same person.
//!
//! Environment variables are never enumerated. `%USERPROFILE%` is read by
//! name because the redaction pass needs its value to remove it; nothing
//! iterates `env::vars()`, whose values carry account, machine and domain
//! names, and in a corporate setting internal UNC paths.

mod redact;
pub mod sections;

// Both `Write`s are needed here and they are different traits: `fmt` for
// `writeln!` into the summary's `String`, `io` for the zip writer's bytes.
use std::fmt::Write as _;
use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};

use serde_json::json;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use crate::error::{CoreError, Result};

/// Bumped only when a field changes meaning or disappears. Adding an
/// optional field is not breaking, exactly as the ini parser already treats
/// its own keys.
///
/// A reader meeting an unknown newer version should behave the way
/// `db::ensure_supported_schema_version` does with a newer database: refuse
/// to interpret structured fields it does not recognize, but never refuse
/// to show `summary.txt`, which has no schema to be incompatible with.
pub const BUNDLE_SCHEMA_VERSION: u32 = 1;

/// How much of the tail of the log to carry. The log has no rotation yet,
/// so it can be arbitrarily long; the end of it is the part describing the
/// session being reported.
const LOG_TAIL_BYTES: usize = 256 * 1024;

/// What the user chose to include beyond the always-present sections.
///
/// Both default to off. Everything a bundle needs to place a failure in a
/// phase is in the always-included set; these two exist for the cases where
/// a reader has to ask, and they are the two that carry the most about the
/// user rather than about the program.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BundleOptions {
    /// Ship real game names instead of `Game 1`, `Game 2`. A library of a
    /// few dozen titles is close to a fingerprint, which is why this is a
    /// choice rather than a default.
    pub include_game_titles: bool,
    /// Ship the deletion journal row by row, not only its aggregate counts.
    /// Capped at [`sections::DETAIL_ROW_CAP`], with the truncation declared
    /// in the payload.
    pub include_operations_detail: bool,
}

/// Everything the generator needs that it cannot discover for itself.
///
/// Paths are passed in rather than resolved here because the app owns the
/// "everything next to the exe" rule, and a test needs to point all of this
/// at a temporary directory.
#[derive(Debug, Clone)]
pub struct BundleInput {
    pub db_path: PathBuf,
    pub settings_path: PathBuf,
    pub rules_path: PathBuf,
    pub log_path: PathBuf,
    pub app_version: String,
    pub elevated: bool,
    /// The live `%USERPROFILE%`, or `None` when it cannot be read. Passed
    /// in for the same reason the paths are: a test must be able to state a
    /// profile that is not the developer's own account.
    pub user_profile: Option<String>,
    pub options: BundleOptions,
}

/// A finished bundle, in memory, before anything touches disk.
///
/// Carries no file name on purpose. The caller picked the destination
/// before this existed, and the identifier worth quoting in a report is the
/// manifest's `generation_id` - a name invented here would be a second,
/// different number for the same thing.
#[derive(Debug)]
pub struct Bundle {
    /// Exactly the bytes of the archive's own `summary.txt` - what the
    /// preview showed, not a second rendering of it.
    pub summary: String,
    pub bytes: Vec<u8>,
}

/// What a caller's progress callback returns: whether to keep going.
///
/// Cancellation is checked between sections rather than inside them. A
/// section is one query and one serialization; making them interruptible
/// would buy milliseconds and cost every projection an early-return path.
pub type KeepGoing = bool;

/// Renders `summary.txt` without building anything else.
///
/// This is what the settings preview shows, and it is the same function the
/// archive's own `summary.txt` comes from - so what the user reads before
/// writing is what gets written, rather than a description of it.
///
/// Deliberately cheap: counts and identity only, no findings projection, so
/// flipping an opt-in toggle can regenerate it inline without a worker.
pub fn summary(input: &BundleInput) -> Result<String> {
    let conn = crate::db::open(&input.db_path)?;
    let (games, files, findings) = sections::counts(&conn)?;
    let health = sections::db_health(&conn)?;
    let roots = sections::library_roots(&conn)?;
    let redactor = redact::Redactor::new(&roots, input.user_profile.as_deref());

    let mut out = String::new();
    let _ = writeln!(out, "GameTrimmer diagnostic bundle");
    let _ = writeln!(out, "  app version:      {}", input.app_version);
    let _ = writeln!(out, "  generated at:     {} UTC", now_utc());
    let _ = writeln!(out, "  elevated:         {}", input.elevated);
    let _ = writeln!(out, "  bundle schema:    {BUNDLE_SCHEMA_VERSION}");
    let _ = writeln!(out);
    let _ = writeln!(out, "Database");
    let _ = writeln!(
        out,
        "  schema version:   {} (this build supports {})",
        health["user_version"], health["current_schema_version"]
    );
    let _ = writeln!(out, "  journal mode:     {}", health["journal_mode"]);
    let _ = writeln!(out, "  integrity check:  {}", health["integrity_check"]);
    let _ = writeln!(out, "  active scan:      {}", health["active_scan_id"]);
    let _ = writeln!(out);
    let _ = writeln!(out, "Active generation");
    let _ = writeln!(out, "  libraries:        {}", roots.len());
    let _ = writeln!(out, "  games:            {games}");
    let _ = writeln!(out, "  files indexed:    {files}");
    let _ = writeln!(out, "  findings:         {findings}");
    let _ = writeln!(out);
    let _ = writeln!(out, "Included in this bundle");
    for name in section_names(&input.options) {
        let _ = writeln!(out, "  {name}");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Privacy");
    let _ = writeln!(
        out,
        "  Library paths are replaced with <LIBRARY_n>; the Windows account"
    );
    let _ = writeln!(
        out,
        "  name is removed everywhere, with no opt-out. Machine GUID, volume"
    );
    let _ = writeln!(
        out,
        "  serials and content hashes are never included. Game titles are"
    );
    let _ = writeln!(
        out,
        "  {}.",
        if input.options.include_game_titles {
            "INCLUDED because you chose to"
        } else {
            "replaced with Game 1, Game 2, ..."
        }
    );
    let _ = writeln!(
        out,
        "  The deletion journal is {}.",
        if input.options.include_operations_detail {
            "included row by row because you chose to"
        } else {
            "included as counts only"
        }
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "This file was generated locally and sent by nobody. GameTrimmer has"
    );
    let _ = writeln!(
        out,
        "no network code: attaching it is a thing you do, not a thing it did."
    );

    // The summary is text like any other section and goes through the same
    // pass - the integrity-check line alone can carry a full path.
    Ok(redactor.apply(&out))
}

/// The archive's entries, in order, for the options given. The manifest's
/// `sections_included` is built from exactly this list, so the two cannot
/// drift.
fn section_names(options: &BundleOptions) -> Vec<&'static str> {
    let mut names = vec![
        "manifest.json",
        "summary.txt",
        "settings.json",
        "rules.json",
        "db_health.json",
        "scan_runs.json",
        "games.json",
        "findings.json",
        "operations_summary.json",
    ];
    if options.include_operations_detail {
        names.push("operations_detail.json");
    }
    names.push("errors.txt");
    names
}

/// Collects, redacts and compresses the whole bundle in memory.
///
/// Returns `Ok(None)` when `progress` asked to stop. Nothing has touched
/// disk at that point and nothing will: the archive exists only as a
/// `Vec<u8>` until [`write`] is called, which is what makes a cancelled run
/// leave no plausible-looking partial file rather than a half-written one.
pub fn build(
    input: &BundleInput,
    progress: &mut dyn FnMut(&str, usize, usize) -> KeepGoing,
) -> Result<Option<Bundle>> {
    let conn = crate::db::open(&input.db_path)?;
    let roots = sections::library_roots(&conn)?;
    let redactor = redact::Redactor::new(&roots, input.user_profile.as_deref());

    let generation_id = uuid::Uuid::new_v4();
    let names = section_names(&input.options);
    let total = names.len();

    let summary_text = summary(input)?;
    let mut entries: Vec<(&'static str, String)> = Vec::with_capacity(total);

    for (index, name) in names.iter().enumerate() {
        if !progress(name, index, total) {
            return Ok(None);
        }
        let body = match *name {
            "manifest.json" => pretty(&json!({
                "schema_version": BUNDLE_SCHEMA_VERSION,
                "generated_at": now_utc(),
                "generation_id": generation_id.to_string(),
                "app_version": input.app_version,
                "elevated": input.elevated,
                "sections_included": names,
                "redaction_applied": true,
            }))?,
            "summary.txt" => summary_text.clone(),
            "settings.json" => pretty(&settings_section(&input.settings_path))?,
            "rules.json" => pretty(&rules_section(&input.rules_path))?,
            "db_health.json" => pretty(&sections::db_health(&conn)?)?,
            "scan_runs.json" => pretty(&sections::scan_runs(&conn)?)?,
            "games.json" => pretty(&sections::games(&conn, input.options.include_game_titles)?)?,
            "findings.json" => pretty(&sections::findings(
                &conn,
                input.options.include_game_titles,
                sections::FINDINGS_SAMPLE,
            )?)?,
            "operations_summary.json" => pretty(&sections::operations_summary(&conn)?)?,
            "operations_detail.json" => pretty(&sections::operations_detail(&conn)?)?,
            "errors.txt" => errors_section(&conn, &input.log_path),
            other => return Err(CoreError::Other(format!("unknown bundle section {other}"))),
        };
        entries.push((name, body));
    }

    // Redaction runs here, over finished text, and not one line earlier.
    // Field-level redaction would miss every path that `rusqlite::Error`
    // and `std::io::Error` embed through `Display` - which is most of the
    // paths in an error-carrying section.
    let bytes = compress(
        entries
            .into_iter()
            .map(|(name, body)| (name, redactor.apply(&body))),
    )?;

    Ok(Some(Bundle {
        summary: summary_text,
        bytes,
    }))
}

/// Writes a built bundle, reusing the app's existing atomic-replace helper.
///
/// The whole archive is handed over as one `&[u8]` rather than streamed:
/// that is what keeps the helper's guarantee intact - a failed or cancelled
/// write leaves either the previous file or nothing, never a truncated zip
/// that looks like a real one. `validate` reopens the bytes as an archive
/// and checks `manifest.json` parses, the same read-back discipline
/// `settings::save_file` already applies to the ini.
pub fn write(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::atomic_file::atomic_write_with_backup(target, bytes, |_path, written| {
        let mut archive = zip::ZipArchive::new(Cursor::new(written))
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        let manifest = archive
            .by_name("manifest.json")
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        serde_json::from_reader::<_, serde_json::Value>(manifest)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        Ok(())
    })
}

/// Builds the archive itself. Per-entry Deflate rather than a solid stream:
/// measured on real payloads the two land within 0.01 % of each other
/// because one section dominates, and per-entry is what lets a recipient
/// extract `summary.txt` alone.
fn compress<'a>(entries: impl Iterator<Item = (&'a str, String)>) -> Result<Vec<u8>> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in entries {
        writer
            .start_file(name, options)
            .map_err(|err| CoreError::Other(format!("zip entry {name}: {err}")))?;
        writer.write_all(body.as_bytes())?;
    }
    let cursor = writer
        .finish()
        .map_err(|err| CoreError::Other(format!("finish zip: {err}")))?;
    Ok(cursor.into_inner())
}

/// The ini as it is on disk, plus how this build parsed it.
///
/// Both halves on purpose: when a setting "didn't stick", the difference
/// between the raw text and the parsed result *is* the finding, and either
/// one alone hides it. The parser is deliberately forgiving and falls back
/// field by field, so a typo'd value looks identical to an absent one in
/// the parsed form.
fn settings_section(path: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(path);
    let parsed = crate::settings::load_file(path)
        .map(|settings| {
            crate::settings::settings_values(&settings)
                .into_iter()
                .map(|(key, value)| (key.to_string(), serde_json::Value::String(value)))
                .collect::<serde_json::Map<_, _>>()
        })
        .unwrap_or_default();

    json!({
        "path": path.to_string_lossy(),
        "raw": raw.as_deref().unwrap_or(""),
        "read_error": raw.as_ref().err().map(|err| err.to_string()),
        "parsed": parsed,
    })
}

/// Rule-pack identity, never its contents.
///
/// A CRC32 and a rule count answer the one question a reader has - is this
/// the pack we shipped, or an edited/imported one - at no privacy cost and
/// without carrying a file the recipient already has. Both packs
/// materialize from the built-ins on first run and are never overwritten,
/// so user edits win permanently and there is otherwise no way to tell
/// after the fact whether the rule that fired was stock.
fn rules_section(path: &Path) -> serde_json::Value {
    let bytes = std::fs::read(path);
    let parsed = bytes
        .as_ref()
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok());
    let (crc, len) = match &bytes {
        Ok(bytes) => (
            Some(format!("{:08x}", crc32fast::hash(bytes))),
            Some(bytes.len()),
        ),
        Err(_) => (None, None),
    };

    json!({
        "path": path.to_string_lossy(),
        "crc32": crc,
        "bytes": len,
        // The file's own declared version, not the one this build supports:
        // a pack from a newer GameTrimmer is exactly the case worth seeing
        // in a report, and it is the case the scan refuses to load.
        "version": parsed.as_ref().and_then(|value| value.get("version").cloned()),
        "supported_version": crate::rules::RULE_PACK_VERSION,
        "rule_count": parsed
            .as_ref()
            .and_then(|value| value.get("rules"))
            .and_then(|rules| rules.as_array().map(|array| array.len())),
        "read_error": bytes.as_ref().err().map(|err| err.to_string()),
        "matches_builtin": bytes
            .as_ref()
            .ok()
            .map(|bytes| crc32fast::hash(bytes) == crc32fast::hash(crate::rules::BUILTIN_RULES_JSON.as_bytes())),
    })
}

/// The tail of the log plus this scan's diagnostics, as text.
///
/// The two belong together: a diagnostic row says a provider or a volume
/// failed, and the log lines around it say what the app was doing at the
/// time. Text rather than JSON because both halves are already prose - and
/// because a reader opens this one to read it, not to parse it.
fn errors_section(conn: &rusqlite::Connection, log_path: &Path) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "== scan diagnostics (active generation) ==");
    match sections::scan_diagnostics(conn) {
        Ok(serde_json::Value::Array(rows)) if rows.is_empty() => {
            let _ = writeln!(out, "(none recorded)");
        }
        Ok(serde_json::Value::Array(rows)) => {
            for row in rows {
                let _ = writeln!(
                    out,
                    "[{}/{}] {} {}",
                    row["provider"].as_str().unwrap_or("?"),
                    row["stage"].as_str().unwrap_or("?"),
                    row["path"].as_str().unwrap_or("-"),
                    row["message"].as_str().unwrap_or(""),
                );
            }
        }
        Ok(_) => {}
        Err(err) => {
            let _ = writeln!(out, "(could not be read: {err})");
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "== log tail (last {LOG_TAIL_BYTES} bytes) ==");
    match std::fs::read(log_path) {
        Ok(bytes) => {
            let start = bytes.len().saturating_sub(LOG_TAIL_BYTES);
            // Lossy on purpose: a log truncated mid-character must not turn
            // into "the log could not be read".
            out.push_str(&String::from_utf8_lossy(&bytes[start..]));
        }
        Err(err) => {
            let _ = writeln!(
                out,
                "(no log file at {}: {err} - logging may be switched off)",
                log_path.display()
            );
        }
    }
    out
}

fn pretty(value: &serde_json::Value) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

/// `YYYY-MM-DD HH:MM:SS`, UTC, no local offset.
///
/// UTC and only UTC: the log carries local wall clock with no timezone
/// while the database carries Unix seconds, and correlating the two on an
/// unknown machine is guesswork. A bundle that adds a third convention
/// would make that worse.
fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests;
