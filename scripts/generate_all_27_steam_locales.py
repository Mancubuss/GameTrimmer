import json
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

with open(r"locales/en.json", "r", encoding="utf-8") as f:
    en_doc = json.load(f)

with open(r"locales/uk.json", "r", encoding="utf-8") as f:
    uk_doc = json.load(f)

en_strings = en_doc["strings"]
all_keys = list(en_strings.keys())

# We will build full translations for all 27 Steam official languages
# Helper to produce localized json files

LOCALES_META = [
    {"id": "de", "name": "German", "native_name": "Deutsch"},
    {"id": "fr", "name": "French", "native_name": "Français"},
    {"id": "it", "name": "Italian", "native_name": "Italiano"},
    {"id": "es", "name": "Spanish", "native_name": "Español"},
    {"id": "es-419", "name": "Spanish (Latin America)", "native_name": "Español (Latinoamérica)"},
    {"id": "pl", "name": "Polish", "native_name": "Polski"},
    {"id": "pt-BR", "name": "Portuguese (Brazil)", "native_name": "Português (Brasil)"},
    {"id": "pt", "name": "Portuguese (Portugal)", "native_name": "Português (Portugal)"},
    {"id": "ja", "name": "Japanese", "native_name": "日本語"},
    {"id": "ko", "name": "Korean", "native_name": "한국어"},
    {"id": "zh-Hans", "name": "Simplified Chinese", "native_name": "简体中文"},
    {"id": "zh-Hant", "name": "Traditional Chinese", "native_name": "繁體中文"},
    {"id": "tr", "name": "Turkish", "native_name": "Türkçe"},
    {"id": "cs", "name": "Czech", "native_name": "Čeština"},
    {"id": "nl", "name": "Dutch", "native_name": "Nederlands"},
    {"id": "sv", "name": "Swedish", "native_name": "Svenska"},
    {"id": "hu", "name": "Hungarian", "native_name": "Magyar"},
    {"id": "da", "name": "Danish", "native_name": "Dansk"},
    {"id": "no", "name": "Norwegian", "native_name": "Norsk"},
    {"id": "fi", "name": "Finnish", "native_name": "Suomi"},
    {"id": "el", "name": "Greek", "native_name": "Ελληνικά"},
    {"id": "ro", "name": "Romanian", "native_name": "Română"},
    {"id": "bg", "name": "Bulgarian", "native_name": "Български"},
    {"id": "vi", "name": "Vietnamese", "native_name": "Tiếng Việt"},
    {"id": "th", "name": "Thai", "native_name": "ไทย"},
    {"id": "ar", "name": "Arabic", "native_name": "العربية"},
    {"id": "ru", "name": "Russian", "native_name": "Русский"},
]

# Let's import our language dictionaries from builder modules
import generate_locales_data as gld

for meta in LOCALES_META:
    loc_id = meta["id"]
    loc_name = meta["name"]
    loc_native = meta["native_name"]
    
    # Get translated strings dictionary
    t_strings = gld.get_strings_for_locale(loc_id, en_strings)
    
    doc = {
        "id": loc_id,
        "name": loc_name,
        "native_name": loc_native,
        "author": "GameTrimmer Community",
        "version": "1.1.0",
        "strings": t_strings
    }
    
    out_file = os.path.join(r"locales", f"{loc_id}.json")
    with open(out_file, "w", encoding="utf-8") as f:
        json.dump(doc, f, ensure_ascii=False, indent=2)
    print(f"Written {out_file} ({loc_native}) - {len(t_strings)} keys")

print(f"\nAll {len(LOCALES_META)} Steam locale files updated successfully.")
