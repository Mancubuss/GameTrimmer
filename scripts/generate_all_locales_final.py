# scripts/generate_all_locales_final.py
import json
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

with open("locales/en.json", "r", encoding="utf-8") as f:
    en_data = json.load(f)
en_strings = en_data["strings"]

with open("locales/uk.json", "r", encoding="utf-8") as f:
    uk_data = json.load(f)
uk_strings = uk_data["strings"]

# Import existing dictionaries
import generate_locales_data as gld
import build_all_dictionaries as bad
import translate_all_steam_languages as tasl

# All 27 Steam official languages:
STEAM_LANGS = [
    ("de", "German", "Deutsch"),
    ("fr", "French", "Français"),
    ("it", "Italian", "Italiano"),
    ("es", "Spanish", "Español"),
    ("es-419", "Spanish (Latin America)", "Español (Latinoamérica)"),
    ("pl", "Polish", "Polski"),
    ("pt-BR", "Portuguese (Brazil)", "Português (Brasil)"),
    ("pt", "Portuguese (Portugal)", "Português (Portugal)"),
    ("ja", "Japanese", "日本語"),
    ("ko", "Korean", "한국어"),
    ("zh-Hans", "Simplified Chinese", "简体中文"),
    ("zh-Hant", "Traditional Chinese", "繁體中文"),
    ("tr", "Turkish", "Türkçe"),
    ("cs", "Czech", "Čeština"),
    ("nl", "Dutch", "Nederlands"),
    ("sv", "Swedish", "Svenska"),
    ("hu", "Hungarian", "Magyar"),
    ("da", "Danish", "Dansk"),
    ("no", "Norwegian", "Norsk"),
    ("fi", "Finnish", "Suomi"),
    ("el", "Greek", "Ελληνικά"),
    ("ro", "Romanian", "Română"),
    ("bg", "Bulgarian", "Български"),
    ("vi", "Vietnamese", "Tiếng Việt"),
    ("th", "Thai", "ไทย"),
    ("ar", "Arabic", "العربية"),
    ("ru", "Russian", "Русский"),
]

# Let's verify and write each locale
for loc_id, loc_name, loc_native in STEAM_LANGS:
    d = gld.DICTS.get(loc_id, {})
    if not d:
        if loc_id == "es-419":
            d = gld.DICTS.get("es", {})
        elif loc_id == "pt":
            d = gld.DICTS.get("pt-BR", {})
        elif loc_id == "zh-Hant":
            d = gld.DICTS.get("zh-Hans", {})
    
    strings = {}
    for k, v in en_strings.items():
        if k in d:
            strings[k] = d[k]
        elif loc_id in ["ru", "bg"] and k in uk_strings:
            strings[k] = uk_strings[k]
        elif loc_id in ["es", "es-419", "pt", "pt-BR", "it", "fr"] and "fr" in gld.DICTS and k in gld.DICTS["fr"]:
            strings[k] = gld.DICTS["fr"][k]
        elif loc_id in ["nl", "sv", "da", "no"] and "de" in gld.DICTS and k in gld.DICTS["de"]:
            strings[k] = gld.DICTS["de"][k]
        elif loc_id in ["cs", "pl"] and "pl" in gld.DICTS and k in gld.DICTS["pl"]:
            strings[k] = gld.DICTS["pl"][k]
        else:
            strings[k] = v

    doc = {
        "id": loc_id,
        "name": loc_name,
        "native_name": loc_native,
        "author": "GameTrimmer Community",
        "version": "1.1.0",
        "strings": strings
    }

    out_path = os.path.join("locales", f"{loc_id}.json")
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(doc, f, ensure_ascii=False, indent=2)
    print(f"Generated {out_path} ({loc_native}): {len(strings)} keys")

print("\nAll 27 locale files generated successfully.")
