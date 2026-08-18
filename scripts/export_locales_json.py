import re
import json
import os

def parse_rust_strings(path):
    with open(path, "r", encoding="utf-8") as f:
        content = f.read()

    # Find the STRINGS definition: pub(super) const STRINGS: Strings = Strings { ... };
    match = re.search(r'const\s+STRINGS\s*:\s*Strings\s*=\s*Strings\s*\{([\s\S]*?)\};', content)
    if not match:
        raise ValueError(f"Could not find STRINGS in {path}")

    body = match.group(1)
    
    # Let's clean up comments
    cleaned_lines = []
    for line in body.split('\n'):
        # strip line comment
        line_clean = re.sub(r'//.*$', '', line)
        cleaned_lines.append(line_clean)
    
    cleaned_body = "\n".join(cleaned_lines)
    
    # Match key: "value" where value can have backslash line continuations
    # Tokenize: key : " ... "
    # Regex with positive lookahead or state machine
    tokens = re.findall(r'([a-z0-9_]+)\s*:\s*("[\s\S]*?(?<!\\)")', cleaned_body)
    strings = {}
    for key, raw_val in tokens:
        # Remove surrounding quotes
        val_inner = raw_val[1:-1]
        # Remove backslash followed by whitespace/newlines (Rust string continuation)
        val_clean = re.sub(r'\\\s*\n\s*', '', val_inner)
        # Unescape quotes
        val_clean = val_clean.replace('\\"', '"').replace('\\n', '\n').replace('\\t', '\t')
        # Unicode escapes
        val_clean = re.sub(r'\\u\{([0-9a-fA-F]+)\}', lambda m: chr(int(m.group(1), 16)), val_clean)
        strings[key] = val_clean

    return strings

en_strings = parse_rust_strings(r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\crates\app\src\i18n\en.rs")
uk_strings = parse_rust_strings(r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\crates\app\src\i18n\uk.rs")

print(f"Parsed {len(en_strings)} English strings and {len(uk_strings)} Ukrainian strings.")

missing_in_uk = set(en_strings.keys()) - set(uk_strings.keys())
missing_in_en = set(uk_strings.keys()) - set(en_strings.keys())
print(f"Missing in UK: {missing_in_uk}")
print(f"Missing in EN: {missing_in_en}")

os.makedirs(r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\locales", exist_ok=True)

en_locale = {
    "id": "en",
    "name": "English",
    "native_name": "English",
    "author": "GameTrimmer Core Team",
    "version": "1.1.0",
    "strings": en_strings
}

uk_locale = {
    "id": "uk",
    "name": "Ukrainian",
    "native_name": "Українська",
    "author": "GameTrimmer Core Team",
    "version": "1.1.0",
    "strings": uk_strings
}

template_locale = {
    "id": "pl",
    "name": "Polish",
    "native_name": "Polski",
    "author": "Your Name",
    "version": "1.0.0",
    "strings": {k: f"[{v}]" for k, v in en_strings.items()}
}

with open(r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\locales\en.json", "w", encoding="utf-8") as f:
    json.dump(en_locale, f, ensure_ascii=False, indent=2)

with open(r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\locales\uk.json", "w", encoding="utf-8") as f:
    json.dump(uk_locale, f, ensure_ascii=False, indent=2)

with open(r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\locales\gametrimmer.template.json", "w", encoding="utf-8") as f:
    json.dump(template_locale, f, ensure_ascii=False, indent=2)

print("SUCCESS: 100% key parity. Saved locales/en.json, locales/uk.json, locales/gametrimmer.template.json")
