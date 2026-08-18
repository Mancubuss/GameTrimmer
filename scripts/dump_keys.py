import json
import sys

sys.stdout.reconfigure(encoding='utf-8')

with open('locales/en.json', 'r', encoding='utf-8') as f:
    en = json.load(f)
with open('locales/uk.json', 'r', encoding='utf-8') as f:
    uk = json.load(f)

print(f"Total EN keys: {len(en['strings'])}")
print(f"Total UK keys: {len(uk['strings'])}")

for i, (k, v) in enumerate(en['strings'].items()):
    uk_v = uk['strings'].get(k, '<MISSING>')
    print(f"[{i+1}] {k}")
    print(f"  EN: {v}")
    print(f"  UK: {uk_v}")
