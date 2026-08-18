import json
import glob

en = json.load(open('locales/en.json', encoding='utf-8'))['strings']
canonical = set(en.keys())
print(f"Total canonical keys in EN: {len(canonical)}")

for f in sorted(glob.glob('locales/*.json')):
    if 'template' in f:
        continue
    d = json.load(open(f, encoding='utf-8'))
    s = d.get('strings', {})
    diff = canonical - set(s.keys())
    lang_id = d.get('id', f)
    print(f"{lang_id:<10}: {len(s)} keys (missing: {len(diff)})")
