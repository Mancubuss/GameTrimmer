//! CSV export of the current findings tree, for external review and
//! testing. Building the CSV text is a pure function of `findings`/`tree` so
//! it can be unit-tested without touching the filesystem; only
//! [`write_export`] does I/O.

use std::io::Write;
use std::path::Path;

use crate::i18n::{self, Lang};
use crate::model::{
    category_display, disk_label, format_size, is_orphan_branch, source_key, DisplayCategory,
    FindingItem, TopGroup, TreeNode,
};

/// Builds the full CSV text (UTF-8 with a leading BOM, `;`-separated, CRLF
/// line endings) for the current findings tree. One row per non-removed
/// finding (already guaranteed by `tree`, which `model::build_tree` builds
/// only from non-removed items), emitted in tree order: for each top-level
/// branch, game and category, folder nodes' member findings first (inheriting
/// the *node's* category, so the export mirrors the on-screen grouping even
/// when a finding's own granular source maps to a different display
/// category), then orphan file nodes.
///
/// The row *order* follows the tree, so it follows the active grouping axis
/// (`model::GroupAxis`) - the export is a snapshot of what is on screen. The
/// row *content* does not: the Disk column is read from each finding's own
/// install directory rather than from the branch it happens to sit under, so
/// grouping by launcher cannot make the column report "steam" as a disk.
pub fn export_csv(lang: Lang, findings: &[FindingItem], tree: &[TopGroup]) -> String {
    let mut out = String::from('\u{FEFF}');
    out.push_str(i18n::csv_header(lang));
    out.push_str("\r\n");

    for top_group in tree {
        for game in &top_group.games {
            // Orphan-branch findings have no real game name (see
            // `ORPHAN_GAME_ID`); label the CSV's Game column with the localized
            // branch heading so an exported orphan row isn't left blank there.
            let game_name = if is_orphan_branch(game.game_id) {
                i18n::strings(lang).orphan_branch_label
            } else {
                game.game_name.as_str()
            };
            for category_node in &game.categories {
                for node in &category_node.nodes {
                    match node {
                        TreeNode::Folder {
                            group_dir,
                            item_indices,
                            ..
                        } => {
                            for &index in item_indices {
                                push_row(
                                    lang,
                                    &mut out,
                                    category_node.category,
                                    game_name,
                                    Some(group_dir.as_str()),
                                    &findings[index],
                                );
                            }
                        }
                        TreeNode::File { index } => {
                            push_row(
                                lang,
                                &mut out,
                                category_node.category,
                                game_name,
                                None,
                                &findings[*index],
                            );
                        }
                    }
                }
            }
        }
    }

    out
}

/// Appends one CSV row (including its trailing CRLF) for a single finding.
///
/// The Disk column is derived from the finding's own install directory, not
/// passed in from the branch it is nested under: the disk is a property of the
/// file, so changing how the tree is grouped must not change what the export
/// says about it.
///
/// `category` is the tree node's, so a folder's members inherit the node's
/// category and the CSV mirrors the on-screen grouping. `None` is the flat
/// axis's category-less node (see `model::CategoryNode::category`), where
/// there is no grouping to mirror and each finding answers for itself.
fn push_row(
    lang: Lang,
    out: &mut String,
    category: Option<DisplayCategory>,
    game_name: &str,
    group_dir: Option<&str>,
    item: &FindingItem,
) {
    let row = &item.row;
    let s = i18n::strings(lang);
    let category = category.unwrap_or_else(|| row.display_category());
    let fields = [
        disk_label(&row.install_dir),
        category_display(lang, category).to_string(),
        game_name.to_string(),
        group_dir.unwrap_or("").to_string(),
        row.rel_path.clone(),
        row.size.to_string(),
        format_size(lang, row.size),
        row.confidence.to_string(),
        source_key(row.source).to_string(),
        row.rule_desc.clone(),
        row.lang_tag.clone().unwrap_or_default(),
        (if item.selected { s.csv_yes } else { s.csv_no }).to_string(),
    ];

    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.push(';');
        }
        out.push_str(&quote_field(field));
    }
    out.push_str("\r\n");
}

/// Quotes a CSV field with `"..."` (doubling any inner quotes) whenever it
/// contains the separator, a quote character, or a newline.
fn quote_field(field: &str) -> String {
    if field.contains(';') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Writes the already-built CSV text to `path`, overwriting any existing
/// file. Called from a background thread after the save dialog returns.
pub fn write_export(path: &Path, csv: &str) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(csv.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{build_tree, FindingRow, FindingSource, GroupAxis};
    use gametrimmer_core::rules::Category;
    use std::path::PathBuf;

    fn finding(
        game_name: &str,
        rel_path: &str,
        group_dir: Option<&str>,
        size: u64,
        selected: bool,
    ) -> FindingItem {
        FindingItem {
            row: FindingRow {
                file_id: 0,
                game_id: 1,
                game_name: game_name.to_string(),
                install_dir: PathBuf::from("C:\\Games\\Test"),
                rel_path: rel_path.to_string(),
                size,
                size_on_disk: size,
                source: FindingSource::Rule(Category::Bonus),
                rule_desc: "test; rule".to_string(),
                confidence: 90,
                lang_tag: None,
                group_dir: group_dir.map(|s| s.to_string()),
                deletion_block_reason: None,
                imported_untrusted: false,
                library: None,
            },
            selected,
            removed: false,
        }
    }

    #[test]
    fn export_csv_starts_with_a_bom() {
        let items = vec![finding("Game A", "file.txt", None, 10, true)];
        let tree = build_tree(&items, GroupAxis::Disk);

        let csv = export_csv(Lang::Uk, &items, &tree);

        assert!(csv.starts_with('\u{FEFF}'));
    }

    #[test]
    fn export_csv_header_is_exact() {
        let items: Vec<FindingItem> = Vec::new();
        let tree = build_tree(&items, GroupAxis::Disk);

        let csv = export_csv(Lang::Uk, &items, &tree);
        let header_line = csv.trim_start_matches('\u{FEFF}').lines().next().unwrap();

        assert_eq!(header_line, i18n::csv_header(Lang::Uk));
    }

    #[test]
    fn export_csv_quotes_a_field_containing_the_separator() {
        // `rule_desc` is "test; rule" in the fixture, which contains `;`.
        let items = vec![finding("Game A", "file.txt", None, 10, true)];
        let tree = build_tree(&items, GroupAxis::Disk);

        let csv = export_csv(Lang::Uk, &items, &tree);

        assert!(
            csv.contains("\"test; rule\""),
            "field containing ';' must be quoted: {csv}"
        );
    }

    #[test]
    fn export_csv_row_count_matches_non_removed_findings() {
        let mut items = vec![
            finding("Game A", "a.txt", None, 10, true),
            finding("Game A", "b.txt", None, 20, false),
        ];
        items[1].removed = true;
        let tree = build_tree(&items, GroupAxis::Disk);

        let csv = export_csv(Lang::Uk, &items, &tree);
        let data_row_count = csv.lines().count() - 1; // minus the header

        assert_eq!(
            data_row_count, 1,
            "the removed finding must not appear in the export"
        );
    }

    #[test]
    fn export_csv_folder_members_inherit_the_node_category() {
        // A folder with mixed sources: byte majority puts the whole folder
        // (both members) under Docs - `poster.jpg`'s own source is Bonus,
        // but its exported category must still read Docs.
        let mut docs_finding = finding("Game A", "extras\\manual.pdf", Some("extras"), 100, true);
        docs_finding.row.source = FindingSource::Rule(Category::DocsFile);
        let mut bonus_finding = finding("Game A", "extras\\poster.jpg", Some("extras"), 10, true);
        bonus_finding.row.source = FindingSource::Rule(Category::Bonus);
        let items = vec![docs_finding, bonus_finding];
        let tree = build_tree(&items, GroupAxis::Disk);

        let csv = export_csv(Lang::Uk, &items, &tree);

        for line in csv.lines().skip(1) {
            if line.is_empty() {
                continue;
            }
            assert!(
                line.contains("Документація і довідкові матеріали"),
                "every member row must carry the folder node's category: {line}"
            );
        }
    }
}
