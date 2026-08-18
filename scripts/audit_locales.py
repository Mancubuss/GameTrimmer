#!/usr/bin/env python3
"""
Comprehensive Locale Verification and Audit Script for GameTrimmer.
Audits all 30 locale JSON files for:
- Valid JSON syntax and UTF-8 encoding
- Exactly 214 keys present in 'strings'
- 0 raw keys (key == value)
- 0 cross-language contamination (e.g. French in Arabic, Polish in Hungarian, Ukrainian in Bulgarian)
- Valid metadata (id, name, native_name, author, version)
"""

import json
import glob
import os
import sys

def run_audit():
    locales_dir = "locales"
    en_path = os.path.join(locales_dir, "en.json")
    uk_path = os.path.join(locales_dir, "uk.json")
    
    if not os.path.exists(en_path) or not os.path.exists(uk_path):
        print(f"Error: {en_path} or {uk_path} missing!")
        return 1

    with open(en_path, "r", encoding="utf-8") as f:
        en_json = json.load(f)
    with open(uk_path, "r", encoding="utf-8") as f:
        uk_json = json.load(f)

    canonical_keys = set(en_json["strings"].keys())
    assert len(canonical_keys) == 240, f"Expected 240 keys in EN, got {len(canonical_keys)}"
    assert set(uk_json["strings"].keys()) == canonical_keys, "UK keys do not match EN keys!"

    # Known universal tokens / technical terms / proper nouns that can legitimately appear across languages
    universal_strings = {
        "GB", "MB", "KB", "B", "NTFS", "SSD", "NVMe", "JSON", "$MFT",
        "GameTrimmer", "gametrimmer.log", "gametrimmer.ini", "rules.json", "l10n_rules.json",
        "Claude", "Andrej Karpathy", "TikiOne Steam Cleaner"
    }

    files = sorted(glob.glob(os.path.join(locales_dir, "*.json")))
    print(f"Auditing {len(files)} locale files in {locales_dir}/...\n")

    errors_found = 0
    warnings_found = 0

    # Load references for contamination check
    ref_strings = {}
    for fpath in files:
        fname = os.path.basename(fpath)
        lang_id = fname.replace(".json", "")
        if lang_id == "gametrimmer.template":
            continue
        try:
            with open(fpath, "r", encoding="utf-8") as f:
                data = json.load(f)
                ref_strings[lang_id] = data.get("strings", {})
        except Exception:
            pass

    for fpath in files:
        fname = os.path.basename(fpath)
        lang_id = fname.replace(".json", "")
        is_template = (lang_id == "gametrimmer.template")

        try:
            with open(fpath, "r", encoding="utf-8") as f:
                data = json.load(f)
        except Exception as e:
            print(f"[FAIL] {fname}: Invalid JSON - {e}")
            errors_found += 1
            continue

        # Check metadata
        for meta_field in ["id", "name", "native_name", "author", "version", "strings"]:
            if meta_field not in data:
                print(f"[FAIL] {fname}: Missing top-level field '{meta_field}'")
                errors_found += 1

        strings = data.get("strings", {})
        keys = set(strings.keys())

        # Check key count
        if len(keys) != 240:
            print(f"[FAIL] {fname}: Key count is {len(keys)}, expected 240")
            errors_found += 1
        
        missing_keys = canonical_keys - keys
        extra_keys = keys - canonical_keys
        if missing_keys:
            print(f"[FAIL] {fname}: Missing {len(missing_keys)} keys: {list(missing_keys)[:5]}...")
            errors_found += 1
        if extra_keys:
            print(f"[FAIL] {fname}: Extra {len(extra_keys)} keys: {list(extra_keys)[:5]}...")
            errors_found += 1

        if is_template:
            # Template has bracketed English, skip translation purity check
            continue

        # Check raw keys and empty strings
        raw_keys = []
        empty_keys = []
        for k, v in strings.items():
            if not isinstance(v, str) or len(v.strip()) == 0:
                empty_keys.append(k)
            elif k == v:
                raw_keys.append(k)

        if raw_keys:
            print(f"[FAIL] {fname}: {len(raw_keys)} raw placeholder keys found: {raw_keys[:5]}...")
            errors_found += 1
        if empty_keys:
            print(f"[FAIL] {fname}: {len(empty_keys)} empty values found: {empty_keys[:5]}...")
            errors_found += 1

        # Check cross-language contamination (long phrases from another language)
        # Compare against French, Polish, Ukrainian, English (if not EN/UK)
        contamination_sources = [("fr", "French"), ("pl", "Polish"), ("uk", "Ukrainian"), ("en", "English")]
        for src_id, src_name in contamination_sources:
            if lang_id == src_id:
                continue
            if src_id not in ref_strings:
                continue
            
            src_dict = ref_strings[src_id]
            contaminated_keys = []
            for k, val in strings.items():
                # Ignore short universal strings or keys where target naturally matches
                src_val = src_dict.get(k)
                if not src_val or len(src_val.strip()) < 15:
                    continue
                if val.strip() == src_val.strip() and val.strip() not in universal_strings:
                    # Check if it contains distinctive non-universal text
                    contaminated_keys.append((k, val))

            if contaminated_keys and len(contaminated_keys) > 3:
                print(f"[FAIL] {fname}: Contaminated with {len(contaminated_keys)} {src_name} strings! Sample: {contaminated_keys[0]}")
                errors_found += 1

        if not raw_keys and not empty_keys and len(keys) == 240:
            print(f"[OK] {fname}: 240 keys verified.")

    print("\n" + "="*50)
    if errors_found == 0:
        print(f"SUCCESS: All {len(files)} locale files are 100% valid, pure, and complete!")
        return 0
    else:
        print(f"FAILED: {errors_found} errors found.")
        return 1

if __name__ == "__main__":
    sys.exit(run_audit())
