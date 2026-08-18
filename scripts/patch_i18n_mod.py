import re

with open(r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\crates\app\src\i18n\mod.rs", "r", encoding="utf-8") as f:
    content = f.read()

# Extract field names from struct Strings
match = re.search(r'(pub struct Strings \{[\s\S]*?\n\})', content)
if not match:
    raise ValueError("Could not find struct Strings in mod.rs")

struct_full = match.group(1)
# Add #[derive(Debug, Clone, Copy)] to struct Strings
struct_with_derive = "#[derive(Debug, Clone, Copy)]\n" + struct_full

fields = re.findall(r'pub\s+([a-z0-9_]+)\s*:\s*&\'static str', struct_full)
overrides_lines = []
for field in fields:
    overrides_lines.append(f'        if let Some(val) = map.get("{field}") {{ s.{field} = Box::leak(val.clone().into_boxed_str()); }}')

apply_overrides_impl = f"""
impl Strings {{
    pub fn apply_overrides(&self, map: &std::collections::HashMap<String, String>) -> Strings {{
        let mut s = *self;
{chr(10).join(overrides_lines)}
        s
    }}
}}
"""

new_mod_top = """//! Hand-rolled i18n with dynamic community locale support.
//! Plain text lives in [`Strings`]; anything with a count, a path, or an error
//! is a function in [`messages`].
//!
//! External JSON locales in `locales/*.json` (portable root or AppData)
//! are discovered dynamically and merged over the English baseline.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

mod en;
mod messages;
mod system;
mod uk;

pub use gametrimmer_core::settings::Lang;
pub use messages::*;
pub use system::detect as detect_system_language;

pub const EMBEDDED_LOCALE_EN: &str = include_str!("../../../../locales/en.json");
pub const EMBEDDED_LOCALE_UK: &str = include_str!("../../../../locales/uk.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleInfo {
    pub id: String,
    pub name: String,
    pub native_name: String,
    pub is_builtin: bool,
}

#[derive(serde::Deserialize)]
struct LocaleHeader {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    native_name: String,
    #[serde(default)]
    strings: HashMap<String, String>,
}

static LOCALE_CACHE: OnceLock<RwLock<HashMap<String, &'static Strings>>> = OnceLock::new();

fn get_locale_cache() -> &'static RwLock<HashMap<String, &'static Strings>> {
    LOCALE_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn scan_dir_locales(dir: &std::path::Path, map: &mut HashMap<String, LocaleInfo>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("json") {
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if file_name.ends_with(".template.json") || file_name.ends_with(".schema.json") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(header) = serde_json::from_str::<LocaleHeader>(&content) {
                    if !header.id.is_empty() {
                        let id = header.id.to_lowercase();
                        let name = if header.name.is_empty() { id.clone() } else { header.name };
                        let native_name = if header.native_name.is_empty() { name.clone() } else { header.native_name };
                        map.insert(id.clone(), LocaleInfo {
                            id,
                            name,
                            native_name,
                            is_builtin: false,
                        });
                    }
                }
            }
        }
    }
}

pub fn available_locales() -> Vec<LocaleInfo> {
    let mut map = HashMap::new();

    // Built-ins
    map.insert("en".to_string(), LocaleInfo {
        id: "en".to_string(),
        name: "English".to_string(),
        native_name: "English".to_string(),
        is_builtin: true,
    });
    map.insert("uk".to_string(), LocaleInfo {
        id: "uk".to_string(),
        name: "Ukrainian".to_string(),
        native_name: "Українська".to_string(),
        is_builtin: true,
    });

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            scan_dir_locales(&exe_dir.join("locales"), &mut map);
        }
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let p = std::path::PathBuf::from(local_app_data).join("GameTrimmer").join("locales");
        scan_dir_locales(&p, &mut map);
    }
    scan_dir_locales(std::path::Path::new("locales"), &mut map);

    let mut list: Vec<LocaleInfo> = map.into_values().collect();
    list.sort_by(|a, b| match (a.id.as_str(), b.id.as_str()) {
        ("en", _) => std::cmp::Ordering::Less,
        (_, "en") => std::cmp::Ordering::Greater,
        ("uk", _) => std::cmp::Ordering::Less,
        (_, "uk") => std::cmp::Ordering::Greater,
        _ => a.id.cmp(&b.id),
    });
    list
}

fn load_external_strings(id: &str) -> Option<Strings> {
    let file_name = format!("{id}.json");

    let candidate_paths = [
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("locales").join(&file_name))),
        std::env::var_os("LOCALAPPDATA").map(|l| std::path::PathBuf::from(l).join("GameTrimmer").join("locales").join(&file_name)),
        Some(std::path::PathBuf::from("locales").join(&file_name)),
    ];

    for path_opt in candidate_paths.into_iter().flatten() {
        if path_opt.exists() {
            if let Ok(content) = std::fs::read_to_string(&path_opt) {
                if let Ok(header) = serde_json::from_str::<LocaleHeader>(&content) {
                    let base = if id == "uk" { &uk::STRINGS } else { &en::STRINGS };
                    return Some(base.apply_overrides(&header.strings));
                }
            }
        }
    }
    None
}

/// Returns the string table for `lang`.
/// Falls back to embedded English for missing keys/locales.
pub fn strings(lang: Lang) -> &'static Strings {
    let tag = lang.as_str();

    let cache = get_locale_cache();
    {
        let r = cache.read().unwrap();
        if let Some(&ptr) = r.get(tag) {
            return ptr;
        }
    }

    if let Some(loaded) = load_external_strings(tag) {
        let leaked: &'static Strings = Box::leak(Box::new(loaded));
        let mut w = cache.write().unwrap();
        w.insert(tag.to_string(), leaked);
        return leaked;
    }

    match lang {
        Lang::Uk => &uk::STRINGS,
        _ => &en::STRINGS,
    }
}
"""

# Find the rest of mod.rs (Verb enum, all_fields, tests)
verb_match = re.search(r'pub enum Verb \{[\s\S]*$', content)
if not verb_match:
    raise ValueError("Could not find Verb in mod.rs")

verb_and_rest = verb_match.group(0)

final_mod_rs = new_mod_top + "\n" + struct_with_derive + "\n" + apply_overrides_impl + "\n" + verb_and_rest

with open(r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\crates\app\src\i18n\mod.rs", "w", encoding="utf-8") as f:
    f.write(final_mod_rs)

print("Updated crates/app/src/i18n/mod.rs successfully.")
