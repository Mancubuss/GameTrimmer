import subprocess
import urllib.request
import json
import re
import sys

sys.stdout.reconfigure(encoding='utf-8')

ps_cmd = """
Add-Type -AssemblyName System.Security
$bytes = [System.IO.File]::ReadAllBytes("E:\\Mancubus\\Projects\\Vibecoding\\Vikunja\\antigravity-agent.api-token.dpapi")
$unprotected = [System.Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
[System.Text.Encoding]::UTF8.GetString($unprotected)
"""

res = subprocess.run(["powershell", "-NoProfile", "-Command", ps_cmd], capture_output=True, text=True, check=True)
token = res.stdout.strip()

page = 1
max_gt = 0
all_gt = []
while True:
    req = urllib.request.Request(f'http://127.0.0.1:3456/api/v2/projects/5/tasks?page={page}&per_page=100', headers={'Authorization': f'Bearer {token}'})
    with urllib.request.urlopen(req) as resp:
        res = json.loads(resp.read().decode('utf-8'))
    items = res.get('items', []) if isinstance(res, dict) else []
    if not items:
        break
    for item in items:
        title = item.get('title', '')
        m = re.search(r'GT-(\d+)', title)
        if m:
            num = int(m.group(1))
            all_gt.append((num, item.get('id'), title))
            if num > max_gt:
                max_gt = num
    if page >= res.get('total_pages', 1):
        break
    page += 1

print(f"Max GT number: GT-{max_gt}")
print("Top 10 highest GT numbers:")
for num, tid, title in sorted(all_gt, key=lambda x: x[0], reverse=True)[:15]:
    print(f"  GT-{num} (task id {tid}): {title}")
