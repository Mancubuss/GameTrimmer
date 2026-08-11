//! UI deletion-state policy. Filesystem authority still lives in core
//! `DeletePlan` preflight; this controller keeps every GUI entry point
//! fail-closed before a confirmation dialog can be opened.

use crate::model::{DisplayCategory, FindingItem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchBlock {
    pub(crate) reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionSummary {
    pub(crate) count: usize,
    pub(crate) bytes_on_disk: u64,
}

pub(crate) fn selected_indices(items: &[FindingItem]) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.selected && !item.removed)
        .map(|(index, _)| index)
        .collect()
}

pub(crate) fn category_indices(items: &[FindingItem], category: DisplayCategory) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| !item.removed && item.row.display_category() == category)
        .map(|(index, _)| index)
        .collect()
}

/// Validates the complete UI batch before any confirmation or worker spawn.
/// A single blocked, removed, stale, or duplicated row rejects the batch.
pub(crate) fn validate_batch(
    items: &[FindingItem],
    indices: &[usize],
) -> Result<Vec<usize>, BatchBlock> {
    let mut checked = Vec::with_capacity(indices.len());
    for &index in indices {
        let Some(item) = items.get(index) else {
            return Err(BatchBlock {
                reason: "the selected row no longer exists".to_string(),
            });
        };
        if item.removed {
            return Err(BatchBlock {
                reason: "the selected row is no longer active".to_string(),
            });
        }
        if checked.contains(&index) {
            return Err(BatchBlock {
                reason: "the delete batch contains a duplicate row".to_string(),
            });
        }
        if let Some(reason) = &item.row.deletion_block_reason {
            return Err(BatchBlock {
                reason: reason.clone(),
            });
        }
        checked.push(index);
    }
    Ok(checked)
}

pub(crate) fn select_all(items: &mut [FindingItem]) {
    for item in items {
        if !item.removed && item.row.deletion_block_reason.is_none() {
            item.selected = true;
        }
    }
}

pub(crate) fn deselect_all(items: &mut [FindingItem]) {
    for item in items {
        item.selected = false;
    }
}

pub(crate) fn selection_summary(items: &[FindingItem]) -> SelectionSummary {
    let mut summary = SelectionSummary {
        count: 0,
        bytes_on_disk: 0,
    };
    for item in items {
        if item.selected && !item.removed {
            summary.count += 1;
            summary.bytes_on_disk += item.row.size_on_disk;
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FindingRow, FindingSource};
    use gametrimmer_core::rules::Category;
    use std::path::PathBuf;

    fn item(file_id: i64, blocked: Option<&str>, selected: bool, removed: bool) -> FindingItem {
        FindingItem {
            row: FindingRow {
                file_id,
                game_id: 1,
                game_name: "Game".to_string(),
                install_dir: PathBuf::from(r"C:\Game"),
                rel_path: format!("{file_id}.bin"),
                size: 10,
                size_on_disk: 16,
                source: FindingSource::Rule(Category::Bonus),
                rule_desc: String::new(),
                confidence: 90,
                lang_tag: None,
                group_dir: None,
                deletion_block_reason: blocked.map(str::to_string),
                imported_untrusted: false,
            },
            selected,
            removed,
        }
    }

    #[test]
    fn one_blocked_row_rejects_the_entire_batch() {
        let items = vec![
            item(1, None, true, false),
            item(2, Some("fresh scan required"), true, false),
        ];

        let error = validate_batch(&items, &[0, 1]).unwrap_err();
        assert_eq!(error.reason, "fresh scan required");
    }

    #[test]
    fn select_all_never_selects_blocked_or_removed_rows() {
        let mut items = vec![
            item(1, None, false, false),
            item(2, Some("blocked"), false, false),
            item(3, None, false, true),
        ];

        select_all(&mut items);

        assert!(items[0].selected);
        assert!(!items[1].selected);
        assert!(!items[2].selected);
    }

    #[test]
    fn selection_summary_uses_live_selected_on_disk_bytes() {
        let items = vec![
            item(1, None, true, false),
            item(2, None, true, true),
            item(3, None, false, false),
        ];

        assert_eq!(
            selection_summary(&items),
            SelectionSummary {
                count: 1,
                bytes_on_disk: 16,
            }
        );
    }
}
