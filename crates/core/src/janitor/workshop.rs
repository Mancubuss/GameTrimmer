//! Steam and EGS Workshop orphan detection (GT-181).
//!
//! Identifies workshop mods left on disk after uninstallation or unsubscription:
//! - Parses `appworkshop_<appid>.acf` to extract subscribed Workshop item IDs.
//! - Checks items in `steamapps/workshop/content/<appid>/<itemid>`.
//! - Flags folders belonging to uninstalled games or items no longer in active subscriptions.

use crate::janitor::JanitorArtifact;
use crate::rules::Category;
use std::collections::HashSet;
use std::path::Path;

/// Parses a Steam `appworkshop_<appid>.acf` file to extract subscribed item IDs.
pub fn parse_subscribed_items(acf_content: &str) -> HashSet<String> {
    let mut subscribed = HashSet::new();
    let mut in_details = false;
    let mut current_item_id: Option<String> = None;
    let mut is_subscribed = false;
    let mut depth = 0;

    for line in acf_content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.is_empty() {
            continue;
        }

        if trimmed == "{" {
            depth += 1;
            continue;
        } else if trimmed == "}" {
            if depth > 0 {
                depth -= 1;
            }
            if depth == 1 && in_details {
                if let Some(item_id) = current_item_id.take() {
                    if is_subscribed {
                        subscribed.insert(item_id);
                    }
                }
            } else if depth == 0 {
                in_details = false;
            }
            continue;
        }

        // Tokenize quotes
        let parts: Vec<&str> = trimmed
            .split('"')
            .filter(|s| !s.trim().is_empty())
            .collect();

        if !parts.is_empty() {
            let key = parts[0].to_ascii_lowercase();
            if key == "workshopitemdetails" {
                in_details = true;
            } else if in_details && depth == 2 {
                // Item block header: "123456789"
                if let Some(prev_id) = current_item_id.take() {
                    if is_subscribed {
                        subscribed.insert(prev_id);
                    }
                }
                current_item_id = Some(parts[0].to_string());
                is_subscribed = false;
            } else if in_details && depth == 3 && parts.len() >= 2 {
                let prop_key = parts[0].to_ascii_lowercase();
                let prop_val = parts[1];
                if prop_key == "subscribedbyuser"
                    && (prop_val == "1" || prop_val.eq_ignore_ascii_case("true"))
                {
                    is_subscribed = true;
                }
            }
        }
    }

    if let Some(item_id) = current_item_id {
        if is_subscribed {
            subscribed.insert(item_id);
        }
    }

    subscribed
}

/// Computes the total size of a directory in bytes.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            } else if p.is_dir() {
                total += dir_size(&p);
            }
        }
    }
    total
}

/// Scans a Steam library's `workshop` folder for orphaned items.
///
/// `library_root`: path to Steam library containing `steamapps/`.
/// `installed_app_ids`: set of currently installed Steam AppIDs.
pub fn scan_steam_workshop_orphans(
    library_root: &Path,
    installed_app_ids: &HashSet<String>,
) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    let workshop_dir = library_root.join("steamapps").join("workshop");
    let content_dir = workshop_dir.join("content");

    if !content_dir.is_dir() {
        return artifacts;
    }

    let Ok(app_entries) = std::fs::read_dir(&content_dir) else {
        return artifacts;
    };

    for app_entry in app_entries.flatten() {
        let app_path = app_entry.path();
        if !app_path.is_dir() {
            continue;
        }

        let app_id = match app_path.file_name().and_then(|n| n.to_str()) {
            Some(name) if name.chars().all(|c| c.is_ascii_digit()) => name.to_string(),
            _ => continue,
        };

        let is_app_installed = installed_app_ids.contains(&app_id);
        let acf_path = workshop_dir.join(format!("appworkshop_{app_id}.acf"));

        let subscribed_items = if acf_path.is_file() {
            std::fs::read_to_string(&acf_path)
                .map(|content| parse_subscribed_items(&content))
                .unwrap_or_default()
        } else {
            HashSet::new()
        };

        if let Ok(item_entries) = std::fs::read_dir(&app_path) {
            for item_entry in item_entries.flatten() {
                let item_path = item_entry.path();
                if !item_path.is_dir() {
                    continue;
                }

                let item_id = match item_path.file_name().and_then(|n| n.to_str()) {
                    Some(name) => name.to_string(),
                    None => continue,
                };

                let is_orphan = if !is_app_installed {
                    // Game is not installed, entire workshop item is orphaned
                    true
                } else if !subscribed_items.is_empty() && !subscribed_items.contains(&item_id) {
                    // Game is installed, but this item is no longer in subscriptions
                    true
                } else {
                    false
                };

                if is_orphan {
                    let size = dir_size(&item_path);
                    let desc = if !is_app_installed {
                        format!("Workshop mod {item_id} for uninstalled game (AppID {app_id})")
                    } else {
                        format!("Unsubscribed workshop mod {item_id} (AppID {app_id})")
                    };

                    artifacts.push(JanitorArtifact {
                        path: item_path,
                        category: Category::WorkshopOrphan,
                        size_bytes: size,
                        description: desc,
                        is_safe_default: true,
                        requires_backup: false,
                        app_id: Some(app_id.clone()),
                        game_title: None,
                    });
                }
            }
        }
    }

    artifacts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_subscribed_items() {
        let sample_acf = r#"
"AppWorkshop"
{
	"appid"		"217140"
	"SizeOnDisk"		"123456"
	"WorkshopItemsInstalled"
	{
		"11111"
		{
			"size"		"100"
		}
		"22222"
		{
			"size"		"200"
		}
	}
	"WorkshopItemDetails"
	{
		"11111"
		{
			"manifest"		"12345"
			"timeupdated"		"1600000000"
			"subscribedbyuser"		"1"
		}
		"22222"
		{
			"manifest"		"67890"
			"timeupdated"		"1600000000"
			"subscribedbyuser"		"0"
		}
	}
}
"#;
        let subscribed = parse_subscribed_items(sample_acf);
        assert!(subscribed.contains("11111"));
        assert!(!subscribed.contains("22222"));
        assert_eq!(subscribed.len(), 1);
    }
}
