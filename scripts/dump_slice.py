import json
import sys

sys.stdout.reconfigure(encoding='utf-8')

with open('locales/en.json', 'r', encoding='utf-8') as f:
    en = json.load(f)['strings']
with open('locales/uk.json', 'r', encoding='utf-8') as f:
    uk = json.load(f)['strings']

start = int(sys.argv[1]) if len(sys.argv) > 1 else 1
end = int(sys.argv[2]) if len(sys.argv) > 2 else len(en)

items = list(en.items())[start-1:end]
for idx, (k, v) in enumerate(items, start=start):
    uk_v = uk.get(k, '')
    print(f"[{idx}] {k}")
    print(f"  EN: {v}")
    print(f"  UK: {uk_v}\n")
