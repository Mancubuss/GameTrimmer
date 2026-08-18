# scripts/verify_batch_locales.py
import json
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

with open('locales/en.json', 'r', encoding='utf-8') as f:
    en_data = json.load(f)
with open('locales/uk.json', 'r', encoding='utf-8') as f:
    uk_data = json.load(f)

en_keys = list(en_data['strings'].keys())
uk_keys = list(uk_data['strings'].keys())
assert en_keys == uk_keys, "EN and UK keys do not match exactly in order/content"

target_locales = [
    ("nl", "Dutch", "Nederlands"),
    ("da", "Danish", "Dansk"),
    ("sv", "Swedish", "Svenska"),
    ("no", "Norwegian", "Norsk"),
    ("fi", "Finnish", "Suomi"),
]

all_passed = True

for lang_id, exp_name, exp_native in target_locales:
    fpath = f"locales/{lang_id}.json"
    print(f"\n==========================================")
    print(f"Verifying: {fpath}")
    print(f"==========================================")
    
    if not os.path.exists(fpath):
        print(f"[FAIL] File {fpath} does not exist!")
        all_passed = False
        continue

    try:
        with open(fpath, "r", encoding="utf-8") as f:
            data = json.load(f)
    except Exception as e:
        print(f"[FAIL] Invalid JSON syntax: {e}")
        all_passed = False
        continue

    # 1. Metadata check
    meta_errors = []
    if data.get("id") != lang_id:
        meta_errors.append(f"id mismatch: expected '{lang_id}', got '{data.get('id')}'")
    if data.get("name") != exp_name:
        meta_errors.append(f"name mismatch: expected '{exp_name}', got '{data.get('name')}'")
    if data.get("native_name") != exp_native:
        meta_errors.append(f"native_name mismatch: expected '{exp_native}', got '{data.get('native_name')}'")
    if data.get("author") != "GameTrimmer Community":
        meta_errors.append(f"author mismatch: expected 'GameTrimmer Community', got '{data.get('author')}'")
    if data.get("version") != "1.1.0":
        meta_errors.append(f"version mismatch: expected '1.1.0', got '{data.get('version')}'")

    if meta_errors:
        print(f"[FAIL] Metadata errors:")
        for err in meta_errors:
            print(f"  - {err}")
        all_passed = False
    else:
        print(f"[OK] Metadata valid.")

    strings = data.get("strings", {})
    str_keys = list(strings.keys())

    # 2. Key count and parity
    if len(str_keys) != len(en_keys):
        print(f"[FAIL] Key count mismatch: expected {len(en_keys)}, got {len(str_keys)}")
        all_passed = False
    else:
        print(f"[OK] Key count: {len(str_keys)} keys (exact match with EN/UK).")

    missing = [k for k in en_keys if k not in strings]
    extra = [k for k in str_keys if k not in en_keys]
    if missing:
        print(f"[FAIL] Missing keys: {missing}")
        all_passed = False
    if extra:
        print(f"[FAIL] Extra keys: {extra}")
        all_passed = False

    # 3. Quality & Purity checks
    raw_keys = []
    empty_keys = []
    for k, v in strings.items():
        if not isinstance(v, str) or len(v.strip()) == 0:
            empty_keys.append(k)
        elif k == v:
            raw_keys.append(k)

    if raw_keys:
        print(f"[FAIL] Raw placeholder keys ({len(raw_keys)}): {raw_keys}")
        all_passed = False
    else:
        print(f"[OK] Zero raw placeholder keys.")

    if empty_keys:
        print(f"[FAIL] Empty value keys ({len(empty_keys)}): {empty_keys}")
        all_passed = False
    else:
        print(f"[OK] Zero empty values.")

    # 4. Spot check samples
    print(f"[INFO] Spot-check samples:")
    for sample_key in ["btn_scan_libraries", "onboarding_heading", "settings_section_monitoring", "disclaimer_heading", "col_sort_hint"]:
        print(f"  - {sample_key}: \"{strings.get(sample_key)}\"")

if all_passed:
    print("\n" + "="*50)
    print("ALL 5 TARGET LOCALES (nl, da, sv, no, fi) PASSED 100% VERIFICATION!")
    print("="*50)
else:
    print("\n" + "="*50)
    print("VERIFICATION FAILED! See errors above.")
    print("="*50)
    sys.exit(1)
