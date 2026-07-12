//! UI-side data model: findings grouped for the tree view, plus selection
//! and formatting helpers. Nothing here touches the database directly.

use std::path::PathBuf;

use gametrimmer_core::rules::Category;

/// One classified file, as produced by the scan worker.
#[derive(Debug, Clone)]
pub struct FindingRow {
    pub file_id: i64,
    pub game_id: i64,
    pub game_name: String,
    pub install_dir: PathBuf,
    pub rel_path: String,
    pub size: u64,
    pub category: Category,
    pub rule_desc: String,
    pub confidence: u8,
}

/// A [`FindingRow`] plus UI-only state. Kept in a flat `Vec` so tree nodes
/// can reference items by index instead of duplicating data.
#[derive(Debug, Clone)]
pub struct FindingItem {
    pub row: FindingRow,
    pub selected: bool,
    /// Set once the file has been successfully moved to the Recycle Bin;
    /// removed items are filtered out of the tree but kept around so the
    /// selection/index model stays stable.
    pub removed: bool,
}

/// Fixed display order for categories in the tree (matches docs/04 §5.5).
pub const CATEGORY_ORDER: [Category; 6] = [
    Category::RedistFolder,
    Category::RedistFile,
    Category::DocsFolder,
    Category::DocsFile,
    Category::Bonus,
    Category::DevLeftovers,
];

/// Human-readable Ukrainian label for a category header.
pub fn category_display(category: Category) -> &'static str {
    match category {
        Category::RedistFolder => "Редистрибутиви (теки)",
        Category::RedistFile => "Редистрибутиви (файли)",
        Category::DocsFolder => "Документація (теки)",
        Category::DocsFile => "Документація (файли)",
        Category::Bonus => "Бонусні матеріали",
        Category::DevLeftovers => "Залишки розробки",
    }
}

/// Stable string key used when persisting a category into `findings.category`.
/// Mirrors the `category` values used in `rules.json`.
pub fn category_key(category: Category) -> &'static str {
    match category {
        Category::RedistFolder => "redist_folder",
        Category::RedistFile => "redist_file",
        Category::DocsFolder => "docs_folder",
        Category::DocsFile => "docs_file",
        Category::Bonus => "bonus",
        Category::DevLeftovers => "dev_leftovers",
    }
}

/// Default selection policy (docs/04 §5.5): auto-select only high-confidence
/// findings; lower-confidence ones are shown but left for the user to opt in.
pub const AUTO_SELECT_CONFIDENCE_THRESHOLD: u8 = 85;

pub fn default_selected(confidence: u8) -> bool {
    confidence >= AUTO_SELECT_CONFIDENCE_THRESHOLD
}

/// One game's findings within a category, holding indices into the flat
/// `findings` vec.
#[derive(Debug, Clone)]
pub struct GameGroup {
    pub game_id: i64,
    pub game_name: String,
    pub item_indices: Vec<usize>,
}

/// One category's games, in display order.
#[derive(Debug, Clone)]
pub struct CategoryGroup {
    pub category: Category,
    pub games: Vec<GameGroup>,
}

/// Rebuilds the category -> game -> items tree from scratch, skipping
/// removed items. Cheap enough to call after every scan/delete completion.
pub fn build_tree(items: &[FindingItem]) -> Vec<CategoryGroup> {
    let mut tree = Vec::new();

    for &category in &CATEGORY_ORDER {
        let mut games: Vec<GameGroup> = Vec::new();

        for (index, item) in items.iter().enumerate() {
            if item.removed || item.row.category != category {
                continue;
            }

            match games.iter_mut().find(|g| g.game_id == item.row.game_id) {
                Some(group) => group.item_indices.push(index),
                None => games.push(GameGroup {
                    game_id: item.row.game_id,
                    game_name: item.row.game_name.clone(),
                    item_indices: vec![index],
                }),
            }
        }

        if games.is_empty() {
            continue;
        }

        games.sort_by(|a, b| a.game_name.cmp(&b.game_name));
        tree.push(CategoryGroup { category, games });
    }

    tree
}

/// Whether every / any item in `indices` is currently selected. Used to
/// drive the tri-state checkbox on category and game headers.
pub fn group_selection_state(items: &[FindingItem], indices: &[usize]) -> (bool, bool) {
    if indices.is_empty() {
        return (false, false);
    }
    let selected_count = indices.iter().filter(|&&i| items[i].selected).count();
    (selected_count == indices.len(), selected_count > 0)
}

/// Flips the selection of a whole group: selects all if not all are
/// currently selected, otherwise deselects all.
pub fn toggle_group(items: &mut [FindingItem], indices: &[usize]) {
    let (all_selected, _) = group_selection_state(items, indices);
    let new_state = !all_selected;
    for &index in indices {
        items[index].selected = new_state;
    }
}

/// Total size in bytes of the selected, non-removed items in `indices`.
pub fn group_size_bytes(items: &[FindingItem], indices: &[usize]) -> u64 {
    indices.iter().map(|&i| items[i].row.size).sum()
}

/// Formats a byte count as a human-readable Ukrainian size string
/// (binary units: 1024-based).
pub fn format_size(bytes: u64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        game_id: i64,
        game_name: &str,
        category: Category,
        confidence: u8,
        size: u64,
    ) -> FindingItem {
        FindingItem {
            row: FindingRow {
                file_id: 0,
                game_id,
                game_name: game_name.to_string(),
                install_dir: PathBuf::from("C:\\Games\\Test"),
                rel_path: "file.txt".to_string(),
                size,
                category,
                rule_desc: "test rule".to_string(),
                confidence,
            },
            selected: default_selected(confidence),
            removed: false,
        }
    }

    #[test]
    fn build_tree_groups_by_category_then_game() {
        let items = vec![
            item(1, "Game A", Category::RedistFolder, 90, 100),
            item(1, "Game A", Category::RedistFile, 95, 50),
            item(2, "Game B", Category::RedistFolder, 90, 200),
        ];

        let tree = build_tree(&items);

        assert_eq!(tree.len(), 2, "two distinct categories present");
        let redist_folder = tree
            .iter()
            .find(|g| g.category == Category::RedistFolder)
            .expect("redist folder category present");
        assert_eq!(
            redist_folder.games.len(),
            2,
            "two distinct games under redist folder"
        );
    }

    #[test]
    fn build_tree_skips_removed_items() {
        let mut items = vec![item(1, "Game A", Category::Bonus, 90, 10)];
        items[0].removed = true;

        let tree = build_tree(&items);

        assert!(tree.is_empty(), "removed items must not appear in the tree");
    }

    #[test]
    fn default_selected_applies_confidence_threshold() {
        assert!(default_selected(85));
        assert!(default_selected(95));
        assert!(!default_selected(84));
    }

    #[test]
    fn toggle_group_selects_all_then_deselects_all() {
        let mut items = vec![
            item(1, "Game A", Category::Bonus, 50, 10),
            item(1, "Game A", Category::Bonus, 50, 10),
        ];
        items[0].selected = false;
        items[1].selected = false;
        let indices = vec![0, 1];

        toggle_group(&mut items, &indices);
        assert!(
            items.iter().all(|i| i.selected),
            "toggling an unselected group selects all"
        );

        toggle_group(&mut items, &indices);
        assert!(
            items.iter().all(|i| !i.selected),
            "toggling a fully selected group deselects all"
        );
    }

    #[test]
    fn group_selection_state_detects_partial_selection() {
        let mut items = vec![
            item(1, "Game A", Category::Bonus, 90, 10),
            item(1, "Game A", Category::Bonus, 50, 10),
        ];
        items[0].selected = true;
        items[1].selected = false;

        let (all, any) = group_selection_state(&items, &[0, 1]);
        assert!(!all);
        assert!(any);
    }

    #[test]
    fn format_size_picks_appropriate_unit() {
        assert_eq!(format_size(512), "512 Б");
        assert_eq!(format_size(2048), "2.00 КБ");
        assert_eq!(format_size(5 * 1024 * 1024), "5.00 МБ");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.00 ГБ");
    }
}
